use std::{collections::HashMap, sync::Arc};

use axum::{extract::State, response::Response, Json};
use base64::Engine;
use musig2::{BinaryEncoding, PubNonce};
use serde::Serialize;
use tracing::instrument;
use via_btc_client::traits::Serializable;

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
            tracing::debug!(
                "Create new {} signing session",
                &session_op.get_session_type()
            );

            let new_session = SigningSession {
                session_op: Some(session_op),
                received_nonces: HashMap::new(),
                received_sigs: HashMap::new(),
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
                    "Invalid partial signature submitted".to_string(),
                ))
            }
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
}
