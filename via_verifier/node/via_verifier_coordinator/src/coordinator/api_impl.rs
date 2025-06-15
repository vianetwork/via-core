use std::{collections::BTreeMap, sync::Arc};

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
    metrics::{MetricSessionType, VerifierErrorLabel, METRICS},
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
                received_nonces: BTreeMap::new(),
                received_sigs: BTreeMap::new(),
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

        let received_nonces = session
            .received_nonces
            .iter()
            .map(|(k, inner_map)| (*k, inner_map.len()))
            .collect();

        let received_partial_signatures = session
            .received_sigs
            .iter()
            .map(|(k, inner_map)| (*k, inner_map.len()))
            .collect();

        return ok_json(SigningSessionResponse {
            session_op: session_op_bytes,
            required_signers: self_.state.required_signers,
            received_nonces,
            received_partial_signatures,
            created_at: session.created_at,
        });
    }

    #[instrument(skip(self_))]
    pub async fn submit_nonce(
        State(self_): State<Arc<Self>>,
        Json(nonce_pair_per_input): Json<BTreeMap<usize, NoncePair>>,
    ) -> anyhow::Result<Response<String>, ApiError> {
        let mut session = self_.state.signing_session.write().await;

        for (input_index, nonce_pair) in nonce_pair_per_input {
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(&nonce_pair.nonce)
                .map_err(|_| ApiError::BadRequest("Invalid base64-encoded nonce".to_string()))?;

            let pub_nonce = PubNonce::from_bytes(&decoded)
                .map_err(|_| ApiError::BadRequest("Invalid public nonce format".to_string()))?;

            let inner_map = session
                .received_nonces
                .entry(input_index)
                .or_insert_with(BTreeMap::new);

            inner_map.insert(nonce_pair.signer_index, pub_nonce);
        }

        Ok(ok_json("Success"))
    }

    #[instrument(skip(self_))]
    pub async fn submit_partial_signature(
        State(self_): State<Arc<Self>>,
        Json(sig_pair_per_input): Json<BTreeMap<usize, PartialSignaturePair>>,
    ) -> anyhow::Result<Response<String>, ApiError> {
        let mut session = self_.state.signing_session.write().await;

        // Temporarily move session_op out to avoid mutable borrow overlap
        let session_op = match session.session_op.as_mut() {
            Some(op) => op,
            None => return Ok(ok_json("no session")),
        };

        let pubkeys_str: Vec<String> = self_
            .state
            .verifiers_pub_keys
            .iter()
            .map(ToString::to_string)
            .collect();

        let messages = session_op.get_message_to_sign();

        for (input_index, sig_pair) in sig_pair_per_input {
            let partial_sig = decode_signature(sig_pair.signature.clone()).map_err(|_| {
                ApiError::BadRequest("Error decoding the partial signature".to_string())
            })?;

            let input_nonces = session.received_nonces.get(&input_index).ok_or_else(|| {
                ApiError::BadRequest(format!(
                    "Session nonce not found for verifier index {}",
                    sig_pair.signer_index
                ))
            })?;

            let nonce = input_nonces.get(&sig_pair.signer_index).ok_or_else(|| {
                ApiError::BadRequest(format!(
                    "Nonce not found for verifier index {}",
                    sig_pair.signer_index
                ))
            })?;

            let all_nonces = input_nonces.values().cloned().collect::<Vec<_>>();

            let individual_pubkey_str = pubkeys_str
                .get(sig_pair.signer_index)
                .cloned()
                .ok_or_else(|| {
                    ApiError::BadRequest("Invalid signer index in pubkey list".to_string())
                })?;

            let message = messages.get(input_index).ok_or_else(|| {
                ApiError::BadRequest("Invalid input index in message list".to_string())
            })?;

            if let Err(e) = verify_partial_signature(
                nonce.clone(),
                all_nonces,
                individual_pubkey_str.clone(),
                pubkeys_str.clone(),
                partial_sig,
                message,
            ) {
                self_.reset_session().await;

                METRICS.verifier_errors[&VerifierErrorLabel {
                    pubkey: individual_pubkey_str.clone(),
                    kind: crate::metrics::ErrorKind::PartialSignature,
                }]
                    .inc();

                tracing::info!("Reset session due to: {}", e);

                return Err(ApiError::BadRequest(format!(
                    "Invalid partial signature for verifier pubkey: {}",
                    individual_pubkey_str
                )));
            }

            session
                .received_sigs
                .entry(input_index)
                .or_insert_with(BTreeMap::new)
                .insert(sig_pair.signer_index, partial_sig);

            tracing::debug!(
                "Valid partial signature submitted by {}",
                individual_pubkey_str
            );
        }
        drop(session);
        Ok(ok_json("Success"))
    }

    #[instrument(skip(self_))]
    pub async fn get_nonces(State(self_): State<Arc<Self>>) -> Response<String> {
        let session = self_.state.signing_session.read().await;

        let mut nonces: BTreeMap<usize, BTreeMap<usize, String>> = BTreeMap::new();

        for (&input_index, signer_nonces) in &session.received_nonces {
            let mut encoded_signer_nonces = BTreeMap::new();

            for (&signer_index, pub_nonce) in signer_nonces {
                let encoded =
                    base64::engine::general_purpose::STANDARD.encode(pub_nonce.to_bytes());
                encoded_signer_nonces.insert(signer_index, encoded);
            }

            nonces.insert(input_index, encoded_signer_nonces);
        }
        drop(session);
        ok_json(nonces)
    }

    #[instrument(skip(self_))]
    pub async fn get_submitted_signatures(State(self_): State<Arc<Self>>) -> Response<String> {
        let session = self_.state.signing_session.read().await;

        let mut signatures: BTreeMap<usize, Vec<PartialSignaturePair>> = BTreeMap::new();

        for (&input_index, sigs_per_signer) in &session.received_sigs {
            let mut encoded_sigs = vec![];

            for (&signer_index, signature) in sigs_per_signer {
                if let Ok(encoded) = encode_signature(signer_index, *signature) {
                    encoded_sigs.push(encoded);
                }
            }

            signatures.insert(input_index, encoded_sigs);
        }
        drop(session);
        ok_json(signatures)
    }

    pub async fn reset_session(&self) {
        let mut session = self.state.signing_session.write().await;
        *session = SigningSession::default();
    }
}
