use std::{collections::HashMap, sync::Arc};

use axum::{extract::State, response::Response, Json};
use base64::Engine;
use bitcoin::{hashes::Hash, Txid};
use musig2::{BinaryEncoding, PartialSignature, PubNonce};
use serde::Serialize;
use tracing::instrument;
use via_musig2::verify_signature;
use zksync_dal::CoreDal;

use super::api_decl::RestApi;
use crate::types::{NoncePair, PartialSignaturePair, SigningSession, SigningSessionResponse};

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

fn internal_server_error(message: &str) -> Response<String> {
    Response::builder()
        .status(axum::http::StatusCode::INTERNAL_SERVER_ERROR)
        .body(message.to_string())
        .unwrap()
}

impl RestApi {
    #[instrument(skip(self_))]

    pub async fn new_session(State(self_): State<Arc<Self>>) -> Response<String> {
        let create_new_session: bool;

        {
            let signing_session = self_.state.signing_session.read().await;
            create_new_session = !signing_session.initialized || signing_session.finished;
        }

        if !create_new_session {
            return bad_request("Session already created");
        }

        let blob_id = "";
        let proof_txid = Txid::all_zeros();
        let block_number = 0;

        let storage = self_
            .master_connection_pool
            .connection_tagged("coordinator")
            .await
            .unwrap()
            .via_votes_dal();

        // Fetch the first ready batch to be processed from db
        // Compute the session_id from the proof_txid
        // Make sure to not create a new session only if the previous one was processed.
        // TODO: fetch the blob_id from db

        let withdrawals = self_
            .withdrawal_client
            .get_withdrawals(blob_id)
            .await
            .unwrap();

        if withdrawals.is_empty() {
            return bad_request("There are no withdrawals to process in this block");
        }

        let unsigned_tx = self_
            .withdrawal_builder
            .create_unsigned_withdrawal_tx(withdrawals, proof_txid)
            .await
            .unwrap();

        let tx_id = unsigned_tx.txid.to_string();

        {
            let mut store_unsigned_tx = self_.state.unsigned_tx.write().await;
            *store_unsigned_tx = Some(unsigned_tx.clone());
        }

        let message = unsigned_tx
            .tx
            .compute_txid()
            .as_raw_hash()
            .as_byte_array()
            .to_vec();

        let new_sesssion = SigningSession {
            block_number,
            tx_id,
            received_nonces: HashMap::new(),
            received_sigs: HashMap::new(),
            final_signature: None,
            message: message.clone(),
            finished: false,
            initialized: true,
        };

        {
            let mut session = self_.state.signing_session.write().await;
            *session = new_sesssion;
        }

        return ok_json(block_number);
    }

    #[instrument(skip(self_))]
    pub async fn get_session(State(self_): State<Arc<Self>>) -> Response<String> {
        let session = self_.state.signing_session.read().await;

        let signer = self_.state.signer.read().await;
        return ok_json(SigningSessionResponse {
            block_number: session.block_number,
            message_to_sign: hex::encode(&session.message),
            aggregated_pubkey: hex::encode(signer.aggregated_pubkey().serialize()),
            required_signers: self_.state.required_signers,
            received_nonces: session.received_nonces.len(),
            received_partial_signatures: session.received_sigs.len(),
            final_signature: session
                .final_signature
                .as_ref()
                .map(|sig| hex::encode(sig.serialize())),
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

        if session.received_nonces.len() == self_.state.required_signers {
            let mut signer = self_.state.signer.write().await;
            for (&i, nonce) in &session.received_nonces {
                if i != 0 {
                    if let Err(_) = signer.receive_nonce(i, nonce.clone()) {
                        return internal_server_error("Failed to process nonce");
                    }
                }
            }
            match signer.create_partial_signature() {
                Ok(partial_sig) => {
                    session.received_sigs.insert(0, partial_sig);
                }
                Err(_) => {
                    return internal_server_error("Failed to create partial signature");
                }
            }
        }

        ok_json("Success")
    }

    #[instrument(skip(self_))]
    pub async fn submit_partial_signature(
        State(self_): State<Arc<Self>>,
        Json(sig_pair): Json<PartialSignaturePair>,
    ) -> Response<String> {
        let decoded_sig =
            match base64::engine::general_purpose::STANDARD.decode(&sig_pair.signature) {
                Ok(sig) => sig,
                Err(_) => return bad_request("Error to decode provided signature"),
            };

        let partial_sig = match PartialSignature::from_slice(&decoded_sig) {
            Ok(sig) => sig,
            Err(_) => return bad_request("Invalid signature"),
        };

        {
            let mut session = self_.state.signing_session.write().await;

            session
                .received_sigs
                .insert(sig_pair.signer_index, partial_sig);

            if session.received_sigs.len() == self_.state.required_signers {
                let mut signer = self_.state.signer.write().await;

                for (&i, psig) in &session.received_sigs {
                    if i != 0 {
                        if let Err(_) = signer.receive_partial_signature(i, *psig) {
                            return internal_server_error("Error to process partial signature");
                        }
                    }
                }

                let final_sig = match signer.create_final_signature() {
                    Ok(sig) => sig,
                    Err(_) => return internal_server_error("Error create final signature"),
                };

                session.final_signature = Some(final_sig);

                // Verify the final signature
                let agg_pub = signer.aggregated_pubkey();
                if let Err(_) = verify_signature(agg_pub, final_sig, &session.message) {
                    return internal_server_error("Error to verifiy the final signature");
                }
            }
        }
        ok_json("Success")
    }

    #[instrument(skip(self_))]
    pub async fn get_final_signature(State(self_): State<Arc<Self>>) -> Response<String> {
        let session = self_.state.signing_session.read().await;

        // Check if the final signature is available
        if let Some(final_sig) = &session.final_signature {
            let serialized_sig = hex::encode(final_sig.serialize());
            return ok_json(serialized_sig);
        }
        not_found("final signature not found")
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
        for (&idx, _) in &session.received_sigs {
            signatures.insert(idx, true);
        }
        ok_json(signatures)
    }
}
