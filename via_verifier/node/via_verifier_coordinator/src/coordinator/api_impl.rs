use super::{api_decl::RestApi, auth_middleware::AuthenticatedRequest, error::ApiError};
use crate::{
    auth::{digest, random_id},
    types::{NoncePair, PartialSignaturePair, SigningSession, SigningSessionResponse},
    utils::{decode_nonce, decode_signature, encode_nonce, encode_signature, seconds_since_epoch},
};
use axum::{extract::State, response::Response, Extension, Json};
use std::{collections::BTreeMap, sync::Arc};
use via_btc_client::traits::Serializable;
use via_musig2::utils::verify_partial_signature;
use via_verifier_dal::VerifierDal;

fn ok_json<T: serde::Serialize>(data: T) -> Result<Response<String>, ApiError> {
    Response::builder()
        .status(200)
        .body(serde_json::to_string(&data).map_err(anyhow::Error::from)?)
        .map_err(|e| ApiError::InternalServerError(e.to_string()))
}

fn check_round(session: &SigningSession, auth: &AuthenticatedRequest) -> Result<usize, ApiError> {
    if session.round_id != auth.binding.round || session.content_hash != auth.binding.content {
        return Err(ApiError::BadRequest("Stale or substituted round".into()));
    }
    session
        .session_op
        .as_ref()
        .map(|op| op.get_message_to_sign().len())
        .ok_or_else(|| ApiError::BadRequest("No active round".into()))
}

fn check_batch<T>(batch: &BTreeMap<usize, T>, count: usize) -> Result<(), ApiError> {
    if count == 0 || batch.len() != count || batch.keys().copied().ne(0..count) {
        return Err(ApiError::BadRequest(
            "Expected complete ordered input batch".into(),
        ));
    }
    Ok(())
}

impl RestApi {
    pub async fn new_session(
        State(this): State<Arc<Self>>,
        Extension(auth): Extension<AuthenticatedRequest>,
    ) -> Result<Response<String>, ApiError> {
        if !this.config.withdrawal_signing_enabled
            || auth.binding.principal != this.config.coordinator_public_key
        {
            return Err(ApiError::Unauthorized(
                "Session creation is disabled or unauthorized".into(),
            ));
        }
        let _creation = this.creation.lock().await;
        let current = this.state.signing_session.read().await.clone();
        if auth.binding.round != current.round_id || auth.binding.content != current.content_hash {
            return Err(ApiError::BadRequest("Stale session creation".into()));
        }
        if let Some(operation) = current.session_op.as_ref() {
            // An unfinished round has no wall-clock expiry: slow peers and fee-admission
            // retries must retain its public transcript and existing exposure reservations.
            // Only the session owner's positive payment observation permits advancement;
            // a no-inclusion observation still leaves exact-byte recovery and holds intact.
            if this
                .session_manager
                .is_session_in_progress(operation)
                .await?
            {
                return ok_json("");
            }
        }
        {
            let authorizer = this
                .session_manager
                .sessions
                .get(&crate::types::SessionType::Withdrawal)
                .and_then(|session| {
                    session
                        .as_any()
                        .downcast_ref::<crate::sessions::withdrawal::WithdrawalSession>()
                })
                .ok_or_else(|| {
                    ApiError::InternalServerError("Missing withdrawal authorizer".into())
                })?;
            let wallet = authorizer.wallet_script().await?;
            let records = this
                .master_connection_pool
                .connection()
                .await?
                .via_withdrawal_dal()
                .list_recoverable_withdrawal_attempts(&wallet)
                .await?;
            for record in records {
                let bound: crate::types::AdmittedContent =
                    bincode::deserialize(&record.content).map_err(anyhow::Error::from)?;
                let operation =
                    bincode::deserialize(&bound.proposal).map_err(anyhow::Error::from)?;
                if !this
                    .session_manager
                    .is_session_in_progress(&operation)
                    .await?
                {
                    continue;
                }
                if bound.wallet != wallet
                    || bound.network != this.network
                    || bound.chain_id != authorizer.chain_id().await?
                    || bound.participants
                        != this
                            .state
                            .verifiers_pub_keys
                            .iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                    || bound.tweak != this.config.bridge_address_merkle_root
                {
                    return Err(ApiError::BadRequest(
                        "Recovered wallet domain changed".into(),
                    ));
                }
                let cached: crate::verifier::CachedShares =
                    serde_json::from_slice(record.public_signatures.as_ref().ok_or_else(|| {
                        ApiError::InternalServerError("Recoverable round missing shares".into())
                    })?)
                    .map_err(anyhow::Error::from)?;
                let mut restored = SigningSession {
                    round_id: record.round_id.as_slice().try_into().map_err(|_| {
                        ApiError::InternalServerError("Invalid durable round".into())
                    })?,
                    content_hash: digest(&record.content),
                    authorized_content: record.content,
                    session_op: Some(operation),
                    created_at: seconds_since_epoch(),
                    ..Default::default()
                };
                for (input, nonces) in cached.nonces {
                    for (signer, nonce) in nonces {
                        restored.received_nonces.entry(input).or_default().insert(
                            signer,
                            decode_nonce(NoncePair {
                                signer_index: signer,
                                nonce,
                            })?,
                        );
                    }
                }
                for (input, pair) in cached.shares {
                    restored
                        .received_sigs
                        .entry(input)
                        .or_default()
                        .insert(pair.signer_index, decode_signature(pair.signature)?);
                }
                *this.state.signing_session.write().await = restored;
                return ok_json("");
            }
        }
        let mut next = SigningSession::default();
        if let Some(op) = this.session_manager.get_next_session().await? {
            next.round_id = random_id();
            let session = this
                .session_manager
                .sessions
                .get(&crate::types::SessionType::Withdrawal)
                .and_then(|session| {
                    session
                        .as_any()
                        .downcast_ref::<crate::sessions::withdrawal::WithdrawalSession>()
                })
                .ok_or_else(|| {
                    ApiError::InternalServerError("Missing withdrawal authorizer".into())
                })?;
            let expected = session.authorize_withdrawal(&op).await?;
            let authorized = crate::types::AdmittedContent {
                proposal: op.to_bytes(),
                network: this.network,
                chain_id: session.chain_id().await?,
                wallet: session.wallet_script().await?,
                participants: this
                    .state
                    .verifiers_pub_keys
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
                tweak: this.config.bridge_address_merkle_root.clone(),
                expected,
            };
            next.authorized_content =
                bincode::serialize(&authorized).map_err(anyhow::Error::from)?;
            let crate::types::SessionOperation::Withdrawal(candidate, ..) = &op;
            this.master_connection_pool
                .connection()
                .await?
                .via_withdrawal_dal()
                .expose_withdrawal_proposal(
                    &authorized.wallet,
                    &next.round_id,
                    &next.authorized_content,
                    &authorized.expected,
                    &candidate.utxos,
                )
                .await?;
            next.content_hash = digest(&next.authorized_content);
            next.session_op = Some(op);
            next.created_at = seconds_since_epoch();
        }
        *this.state.signing_session.write().await = next;
        ok_json("")
    }

    pub async fn get_session(State(this): State<Arc<Self>>) -> Result<Response<String>, ApiError> {
        let session = this.state.signing_session.read().await;
        let nonces = session
            .received_nonces
            .iter()
            .map(|(input, values)| {
                Ok((
                    *input,
                    values
                        .iter()
                        .map(|(signer, nonce)| {
                            Ok((*signer, encode_nonce(*signer, nonce.clone())?.nonce))
                        })
                        .collect::<anyhow::Result<_>>()?,
                ))
            })
            .collect::<anyhow::Result<_>>()?;
        let signatures = session
            .received_sigs
            .iter()
            .map(|(input, values)| {
                Ok((
                    *input,
                    values
                        .iter()
                        .map(|(signer, sig)| {
                            Ok((*signer, encode_signature(*signer, *sig)?.signature))
                        })
                        .collect::<anyhow::Result<_>>()?,
                ))
            })
            .collect::<anyhow::Result<_>>()?;
        ok_json(SigningSessionResponse {
            round_id: session.round_id,
            content_hash: session.content_hash,
            authorized_content: session.authorized_content.clone(),
            nonces,
            signatures,
            session_op: session
                .session_op
                .as_ref()
                .map(Serializable::to_bytes)
                .unwrap_or_default(),
            required_signers: this.state.verifiers_pub_keys.len(),
            received_nonces: session
                .received_nonces
                .iter()
                .map(|(i, m)| (*i, m.len()))
                .collect(),
            received_partial_signatures: session
                .received_sigs
                .iter()
                .map(|(i, m)| (*i, m.len()))
                .collect(),
            created_at: session.created_at,
        })
    }

    pub async fn submit_nonce(
        State(this): State<Arc<Self>>,
        Extension(auth): Extension<AuthenticatedRequest>,
        Json(batch): Json<BTreeMap<usize, NoncePair>>,
    ) -> Result<Response<String>, ApiError> {
        let mut session = this.state.signing_session.write().await;
        let count = check_round(&session, &auth)?;
        check_batch(&batch, count)?;
        let mut decoded = Vec::with_capacity(count);
        for (input, pair) in batch {
            if pair.signer_index != auth.signer {
                return Err(ApiError::Unauthorized("Signer attribution mismatch".into()));
            }
            let nonce = decode_nonce(pair)?;
            if let Some(existing) = session
                .received_nonces
                .get(&input)
                .and_then(|m| m.get(&auth.signer))
            {
                if existing != &nonce {
                    return Err(ApiError::BadRequest("Conflicting nonce replay".into()));
                }
            }
            decoded.push((input, nonce));
        }
        for (input, nonce) in decoded {
            session
                .received_nonces
                .entry(input)
                .or_default()
                .insert(auth.signer, nonce);
        }
        ok_json("")
    }

    pub async fn submit_partial_signature(
        State(this): State<Arc<Self>>,
        Extension(auth): Extension<AuthenticatedRequest>,
        Json(batch): Json<BTreeMap<usize, PartialSignaturePair>>,
    ) -> Result<Response<String>, ApiError> {
        let mut session = this.state.signing_session.write().await;
        let count = check_round(&session, &auth)?;
        check_batch(&batch, count)?;
        let messages = session.session_op.as_ref().unwrap().get_message_to_sign();
        let pubkeys: Vec<_> = this
            .state
            .verifiers_pub_keys
            .iter()
            .map(ToString::to_string)
            .collect();
        let mut decoded = Vec::with_capacity(count);
        for (input, pair) in batch {
            if pair.signer_index != auth.signer {
                return Err(ApiError::Unauthorized("Signer attribution mismatch".into()));
            }
            let signature = decode_signature(pair.signature)?;
            let nonces = session
                .received_nonces
                .get(&input)
                .ok_or_else(|| ApiError::BadRequest("Incomplete nonces".into()))?;
            check_batch(nonces, pubkeys.len())?;
            verify_partial_signature(
                nonces[&auth.signer].clone(),
                nonces.values().cloned().collect(),
                pubkeys[auth.signer].clone(),
                pubkeys.clone(),
                signature,
                &messages[input],
                this.config.bridge_address_merkle_root(),
            )?;
            if let Some(existing) = session
                .received_sigs
                .get(&input)
                .and_then(|m| m.get(&auth.signer))
            {
                if existing != &signature {
                    return Err(ApiError::BadRequest("Conflicting share replay".into()));
                }
            }
            decoded.push((input, signature));
        }
        for (input, signature) in decoded {
            session
                .received_sigs
                .entry(input)
                .or_default()
                .insert(auth.signer, signature);
        }
        ok_json("")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn incomplete_or_sparse_batches_are_rejected() {
        assert!(check_batch(&BTreeMap::from([(0, ()), (2, ())]), 2).is_err());
        assert!(check_batch(&BTreeMap::from([(0, ())]), 2).is_err());
        assert!(check_batch(&BTreeMap::from([(0, ()), (1, ())]), 2).is_ok());
    }
}
