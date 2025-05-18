use std::{collections::HashMap, sync::Arc};

use axum::{extract::State, response::Response, Json};
use base64::Engine;
use musig2::{BinaryEncoding, PubNonce};
use serde::Serialize;
use tracing::instrument;
use via_btc_client::traits::Serializable;
use via_musig2::utils::verify_partial_signature;
use zksync_utils::time::seconds_since_epoch;

use super::{api_decl::RestApi, error::ApiError};
use crate::{
    metrics::{MetricSessionType, METRICS},
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
        let current_session_op_opt = self_.state.signing_session.read().await.session_op.clone();

        if let Some(current_session) = current_session_op_opt {
            if self_
                .session_manager
                .is_session_in_progress(&current_session)
                .await?
            {
                tracing::debug!("Session in progress {}", current_session.get_session_type());
                return Ok(ok_json(""));
            }
        }

        if let Some(session_op) = self_.session_manager.get_next_session().await? {
            tracing::info!(
                "Create new {} signing session",
                &session_op.get_session_type()
            );

            METRICS.session_new[&MetricSessionType::from(session_op.get_session_type())].inc();

            let new_session = SigningSession {
                session_op: Some(session_op),
                received_nonces: HashMap::new(),
                received_sigs: HashMap::new(),
                created_at: seconds_since_epoch(),
            };

            let mut signing_session = self_.state.signing_session.write().await;
            *signing_session = new_session;
        } else {
            self_.reset_session().await;
        }

        return Ok(ok_json(""));
    }

    #[instrument(skip(self_))]
    pub async fn get_session(State(self_): State<Arc<Self>>) -> Response<String> {
        let session = self_.state.signing_session.read().await;

        let mut session_op_bytes = Vec::new();
        if let Some(session_op) = self_.state.signing_session.read().await.session_op.clone() {
            session_op_bytes = session_op.to_bytes();
        };

        return ok_json(SigningSessionResponse {
            session_op: session_op_bytes,
            required_signers: self_.state.required_signers,
            received_nonces: session.received_nonces.len(),
            received_partial_signatures: session.received_sigs.len(),
            created_at: session.created_at,
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
            Err(_) => {
                return Err(ApiError::BadRequest(
                    "Error when decode the partial signature".to_string(),
                ))
            }
        };

        // Verify if the partial sig is valid.
        let session = self_.state.signing_session.read().await.clone();
        if let Some(session_op) = session.session_op {
            let pubkeys_str = self_
                .state
                .verifiers_pub_keys
                .iter()
                .map(|pubkey| pubkey.to_string())
                .collect::<Vec<String>>();

            let individual_pubkey_str = pubkeys_str[sig_pair.signer_index].clone();
            if let Some(nonce) = session.received_nonces.get(&sig_pair.signer_index) {
                let nonces = session
                    .received_nonces
                    .values()
                    .cloned()
                    .collect::<Vec<PubNonce>>();

                match verify_partial_signature(
                    nonce.clone(),
                    nonces,
                    individual_pubkey_str.clone(),
                    pubkeys_str,
                    partial_sig,
                    &session_op.get_message_to_sign(),
                ) {
                    Ok(_) => {}
                    Err(e) => {
                        // Reset the session if a partial signature is not valid.
                        // This will force the verifier to submit a new valid signature.
                        self_.reset_session().await;
                        tracing::info!("Reset session due to: {}", e);
                        return Err(ApiError::BadRequest(
                            format!("Invalid partial signature for verifier pubkey: {individual_pubkey_str}"),
                        ));
                    }
                }
            } else {
                return Err(ApiError::BadRequest(
                    "Session nonce not found for  verifier pubkey: {individual_pubkey_str}"
                        .to_string(),
                ));
            }
        }

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
}
