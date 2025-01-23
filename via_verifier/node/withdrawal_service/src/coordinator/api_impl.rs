use std::{collections::HashMap, sync::Arc};

use anyhow::Context;
use axum::{extract::State, response::Response, Json};
use base64::Engine;
use bitcoin::{
    hashes::Hash,
    sighash::{Prevouts, SighashCache},
    TapSighashType, Txid,
};
use musig2::{BinaryEncoding, PubNonce};
use serde::Serialize;
use tracing::instrument;
use via_btc_client::{traits::Serializable, withdrawal_builder::WithdrawalRequest};
use via_verifier_dal::VerifierDal;
use zksync_types::H256;

use super::{api_decl::RestApi, error::ApiError};
use crate::{
    types::{NoncePair, PartialSignaturePair, SigningSession, SigningSessionResponse},
    utils::{decode_signature, encode_signature},
};

fn ok_json<T: Serialize>(data: T) -> Response<String> {
    Response::builder()
        .status(axum::http::StatusCode::OK)
        .body(serde_json::to_string(&data).expect("Failed to serialize"))
        .unwrap()
}

impl RestApi {
    #[instrument(skip(self_))]
    pub async fn new_session(
        State(self_): State<Arc<Self>>,
    ) -> anyhow::Result<Response<String>, ApiError> {
        let mut l1_block_number: i64;

        {
            let signing_session = self_.state.signing_session.read().await;
            l1_block_number = signing_session.l1_block_number;
        }

        if l1_block_number != 0 {
            let withdrawal_tx = self_
                .master_connection_pool
                .connection_tagged("coordinator api")
                .await?
                .via_votes_dal()
                .get_vote_transaction_withdrawal_tx(l1_block_number)
                .await?;

            if withdrawal_tx.is_none() {
                // The withdrawal process is in progress
                return Ok(ok_json(l1_block_number));
            }
        }

        // Get the l1 batches finilized but withdrawals not yet processed
        let blocks = self_
            .master_connection_pool
            .connection_tagged("coordinator")
            .await
            .unwrap()
            .via_votes_dal()
            .get_finalized_blocks_and_non_processed_withdrawals()
            .await
            .unwrap();

        if blocks.is_empty() {
            if l1_block_number != 0 {
                self_.reset_session().await;
            }
            return Ok(ok_json("No block found for processing withdrawals"));
        }

        let mut withdrawals_to_process: Vec<WithdrawalRequest> = Vec::new();
        let mut proof_txid = Txid::all_zeros();

        for (block_number, blob_id, proof_tx_id) in blocks.iter() {
            let withdrawals = self_
                .withdrawal_client
                .get_withdrawals(blob_id)
                .await
                .context("Error to get withdrawals from DA")?;

            if !withdrawals.is_empty() {
                proof_txid = Self::h256_to_txid(proof_tx_id).context("Invalid proof tx id")?;
                l1_block_number = *block_number;
                withdrawals_to_process = withdrawals;
                break;
            } else {
                // If there is no withdrawals to process in a batch, update the status and mark it as processed
                self_
                    .master_connection_pool
                    .connection_tagged("coordinator")
                    .await?
                    .via_votes_dal()
                    .mark_vote_transaction_as_processed_withdrawals(H256::zero(), *block_number)
                    .await
                    .context("Error to mark a vote transaction as processed")?;
            }
        }

        if withdrawals_to_process.is_empty() {
            self_.reset_session().await;
            return Ok(ok_json("There are no withdrawals to process"));
        }

        let unsigned_tx_result = self_
            .withdrawal_builder
            .create_unsigned_withdrawal_tx(withdrawals_to_process, proof_txid)
            .await;

        let unsigned_tx = match unsigned_tx_result {
            Ok(unsigned_tx) => unsigned_tx,
            Err(err) => {
                tracing::info!("Invalid unsigned tx: {err}");
                return Err(ApiError::InternalServerError(
                    "Invalid unsigned tx".to_string(),
                ));
            }
        };

        let mut sighash_cache = SighashCache::new(&unsigned_tx.tx);
        let sighash_type = TapSighashType::All;
        let mut txout_list = Vec::with_capacity(unsigned_tx.utxos.len());

        for (_, txout) in unsigned_tx.utxos.clone() {
            txout_list.push(txout);
        }
        let sighash = sighash_cache
            .taproot_key_spend_signature_hash(0, &Prevouts::All(&txout_list), sighash_type)
            .unwrap();

        let new_sesssion = SigningSession {
            l1_block_number,
            received_nonces: HashMap::new(),
            received_sigs: HashMap::new(),
            message: sighash.to_byte_array().to_vec(),
            unsigned_tx: Some(unsigned_tx),
        };

        {
            let mut session = self_.state.signing_session.write().await;
            *session = new_sesssion;
        }

        return Ok(ok_json(l1_block_number));
    }

    #[instrument(skip(self_))]
    pub async fn get_session(State(self_): State<Arc<Self>>) -> Response<String> {
        let session = self_.state.signing_session.read().await;

        let mut unsigned_tx_bytes = Vec::new();
        if let Some(unsigned_tx) = self_.state.signing_session.read().await.unsigned_tx.clone() {
            unsigned_tx_bytes = unsigned_tx.to_bytes();
        };

        return ok_json(SigningSessionResponse {
            l1_block_number: session.l1_block_number,
            message_to_sign: hex::encode(&session.message),
            required_signers: self_.state.required_signers,
            received_nonces: session.received_nonces.len(),
            received_partial_signatures: session.received_sigs.len(),
            unsigned_tx: unsigned_tx_bytes,
        });
    }

    #[instrument(skip(self_))]
    pub async fn submit_nonce(
        State(self_): State<Arc<Self>>,
        Json(nonce_pair): Json<NoncePair>,
    ) -> anyhow::Result<Response<String>, ApiError> {
        let decoded_nonce =
            match base64::engine::general_purpose::STANDARD.decode(&nonce_pair.nonce) {
                Ok(nonce) => nonce,
                Err(_) => return Err(ApiError::BadRequest("Invalid nonce pair".to_string())),
            };

        let pub_nonce = match PubNonce::from_bytes(&decoded_nonce) {
            Ok(nonce) => nonce,
            Err(_) => return Err(ApiError::BadRequest("Invalid pub nonce".to_string())),
        };

        let mut session = self_.state.signing_session.write().await;

        session
            .received_nonces
            .insert(nonce_pair.signer_index, pub_nonce);

        Ok(ok_json("Success"))
    }

    #[instrument(skip(self_))]
    pub async fn submit_partial_signature(
        State(self_): State<Arc<Self>>,
        Json(sig_pair): Json<PartialSignaturePair>,
    ) -> anyhow::Result<Response<String>, ApiError> {
        let partial_sig = match decode_signature(sig_pair.signature) {
            Ok(sig) => sig,
            Err(_) => return Err(ApiError::BadRequest("Invalid signature".to_string())),
        };

        {
            let mut session = self_.state.signing_session.write().await;

            session
                .received_sigs
                .insert(sig_pair.signer_index, partial_sig);
        }
        Ok(ok_json("Success"))
    }

    #[instrument(skip(self_))]
    pub async fn get_nonces(State(self_): State<Arc<Self>>) -> Response<String> {
        let session = self_.state.signing_session.read().await;

        let mut nonces = HashMap::new();
        for (&idx, nonce) in &session.received_nonces {
            nonces.insert(
                idx,
                base64::engine::general_purpose::STANDARD.encode(nonce.to_bytes()),
            );
        }
        ok_json(nonces)
    }

    #[instrument(skip(self_))]
    pub async fn get_submitted_signatures(State(self_): State<Arc<Self>>) -> Response<String> {
        let session = self_.state.signing_session.read().await;
        let mut signatures = HashMap::new();
        for (&signer_index, signature) in &session.received_sigs {
            let sig = encode_signature(signer_index, *signature).unwrap();
            signatures.insert(signer_index, sig);
        }
        ok_json(signatures)
    }

    pub async fn reset_session(&self) {
        let mut session = self.state.signing_session.write().await;
        *session = SigningSession::default();
    }

    // Todo: move this logic in a helper crate as it's used in multiple crates.
    /// Converts H256 bytes (from the DB) to a Txid by reversing the byte order.
    fn h256_to_txid(h256_bytes: &[u8]) -> anyhow::Result<Txid> {
        if h256_bytes.len() != 32 {
            return Err(anyhow::anyhow!("H256 must be 32 bytes"));
        }
        let mut reversed_bytes = h256_bytes.to_vec();
        reversed_bytes.reverse();
        Txid::from_slice(&reversed_bytes).context("Failed to convert H256 to Txid")
    }
}
