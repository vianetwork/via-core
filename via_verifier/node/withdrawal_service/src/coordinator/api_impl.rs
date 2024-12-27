use std::{collections::HashMap, sync::Arc};

use anyhow::Context;
use axum::{extract::State, response::Response, Json};
use base64::Engine;
use bitcoin::{hashes::Hash, Txid};
use musig2::{BinaryEncoding, PubNonce};
use serde::Serialize;
use tracing::instrument;
use via_btc_client::{traits::Serializable, withdrawal_builder::WithdrawalRequest};
use zksync_dal::CoreDal;
use zksync_types::H256;

use super::api_decl::RestApi;
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

fn bad_request(message: &str) -> Response<String> {
    Response::builder()
        .status(axum::http::StatusCode::BAD_REQUEST)
        .body(message.to_string())
        .unwrap()
}

fn not_found(message: &str) -> Response<String> {
    Response::builder()
        .status(axum::http::StatusCode::NOT_FOUND)
        .body(message.to_string())
        .unwrap()
}

impl RestApi {
    #[instrument(skip(self_))]
    pub async fn new_session(State(self_): State<Arc<Self>>) -> Response<String> {
        let mut l1_block_number: i64;

        {
            let signing_session = self_.state.signing_session.read().await;
            l1_block_number = signing_session.l1_block_number;
        }

        if l1_block_number != 0 {
            let withdrawal_tx = self_
                .master_connection_pool
                .connection_tagged("coordinator")
                .await
                .unwrap()
                .via_votes_dal()
                .get_vote_transaction_withdrawal_tx(l1_block_number)
                .await
                .unwrap();

            if withdrawal_tx.is_none() {
                // The withdrawal process is in progress
                return ok_json(l1_block_number);
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

        if blocks.len() == 0 {
            return not_found("No block ready to process withdrawals found");
        }

        let mut withdrawals_to_process: Vec<WithdrawalRequest> = Vec::new();
        let mut proof_txid = Txid::all_zeros();

        for i in 0..blocks.len() {
            let (block_number, blob_id, proof_tx) = &blocks[i];
            let withdrawals = self_
                .withdrawal_client
                .get_withdrawals(&blob_id)
                .await
                .unwrap();

            if withdrawals.len() > 0 {
                proof_txid = Txid::from_slice(proof_tx.as_bytes())
                    .context("Invalid proof id")
                    .unwrap();
                l1_block_number = block_number.clone();
                withdrawals_to_process = withdrawals;
                break;
            } else {
                // If there is no withdrawals to process in a batch, update the status and mark it as processed
                _ = self_
                    .master_connection_pool
                    .connection_tagged("coordinator")
                    .await
                    .unwrap()
                    .via_votes_dal()
                    .mark_vote_transaction_as_processed_withdrawals(
                        H256::zero(),
                        l1_block_number.clone(),
                    )
            }
        }

        if withdrawals_to_process.is_empty() {
            {
                let mut session = self_.state.signing_session.write().await;
                *session = SigningSession::default();
            }
            return bad_request("There are no withdrawals to process in this block");
        }

        let unsigned_tx = self_
            .withdrawal_builder
            .create_unsigned_withdrawal_tx(withdrawals_to_process, proof_txid)
            .await
            .unwrap();

        let message = unsigned_tx
            .tx
            .compute_txid()
            .as_raw_hash()
            .as_byte_array()
            .to_vec();

        let new_sesssion = SigningSession {
            l1_block_number,
            received_nonces: HashMap::new(),
            received_sigs: HashMap::new(),
            message: message.clone(),
            unsigned_tx: Some(unsigned_tx),
        };

        {
            let mut session = self_.state.signing_session.write().await;
            *session = new_sesssion;
        }

        return ok_json(l1_block_number);
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
    ) -> Response<String> {
        let decoded_nonce =
            match base64::engine::general_purpose::STANDARD.decode(&nonce_pair.nonce) {
                Ok(nonce) => nonce,
                Err(_) => return bad_request("Invalid nonce pair"),
            };

        let pub_nonce = match PubNonce::from_bytes(&decoded_nonce) {
            Ok(nonce) => nonce,
            Err(_) => return bad_request("Invalid pub nonce"),
        };

        let mut session = self_.state.signing_session.write().await;

        session
            .received_nonces
            .insert(nonce_pair.signer_index, pub_nonce);

        ok_json("Success")
    }

    #[instrument(skip(self_))]
    pub async fn submit_partial_signature(
        State(self_): State<Arc<Self>>,
        Json(sig_pair): Json<PartialSignaturePair>,
    ) -> Response<String> {
        let partial_sig = match decode_signature(sig_pair.signature) {
            Ok(sig) => sig,
            Err(_) => return bad_request("Invalid signature"),
        };

        {
            let mut session = self_.state.signing_session.write().await;

            session
                .received_sigs
                .insert(sig_pair.signer_index, partial_sig);
        }
        ok_json("Success")
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
            let sig = encode_signature(signer_index, signature.clone()).unwrap();
            signatures.insert(signer_index, sig);
        }
        ok_json(signatures)
    }
}
