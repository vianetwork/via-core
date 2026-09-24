use crate::{
    auth::{Binding, Envelope, MAX_BODY},
    coordinator::{api_decl::RestApi, error::ApiError},
};
use axum::{
    body::{self, Body},
    extract::{OriginalUri, Request, State},
    middleware::Next,
    response::Response,
};
use std::str::FromStr;
use std::sync::Arc;
use via_verifier_dal::VerifierDal;

#[derive(Clone)]
pub struct AuthenticatedRequest {
    pub signer: usize,
    pub binding: Binding,
}

pub async fn auth_middleware(
    State(state): State<Arc<RestApi>>,
    request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let method = request.method().to_string();
    let target = request
        .extensions()
        .get::<OriginalUri>()
        .map(|uri| uri.0.to_string())
        .unwrap_or_else(|| request.uri().to_string());
    let (mut parts, body) = request.into_parts();
    let bytes = body::to_bytes(body, MAX_BODY)
        .await
        .map_err(|_| ApiError::BadRequest("Oversized authentication body".into()))?;
    let envelope: Envelope = serde_json::from_slice(&bytes)
        .map_err(|_| ApiError::Unauthorized("Missing authenticated envelope".into()))?;
    let signer = state
        .state
        .verifiers_pub_keys
        .iter()
        .position(|key| key.to_string() == envelope.binding.principal)
        .ok_or_else(|| ApiError::Unauthorized("Unknown principal".into()))?;
    envelope
        .verify(
            &state.state.verifiers_pub_keys[signer],
            chrono::Utc::now().timestamp(),
            state.config.verifier_request_timeout,
        )
        .map_err(|e| ApiError::Unauthorized(e.to_string()))?;
    let version = zksync_types::protocol_version::ProtocolSemanticVersion::from_str(
        &envelope.binding.sequencer_version,
    )?;
    if let Some(latest) = state
        .master_connection_pool
        .connection()
        .await?
        .via_protocol_versions_dal()
        .latest_protocol_semantic_version()
        .await?
    {
        if version < latest {
            return Err(ApiError::Unauthorized("Outdated sequencer version".into()));
        }
    }
    if envelope.binding.method != method
        || envelope.binding.target != target
        || envelope.binding.audience != state.config.coordinator_public_key
        || envelope.binding.status != 0
    {
        return Err(ApiError::Unauthorized("Wrong request context".into()));
    }
    {
        let mut challenges = state.challenges.lock().await;
        let now = chrono::Utc::now().timestamp();
        challenges
            .retain(|_, time| now - *time <= i64::from(state.config.verifier_request_timeout) + 30);
        if challenges
            .insert((signer, envelope.binding.challenge), now)
            .is_some()
        {
            return Err(ApiError::Unauthorized("Replayed request".into()));
        }
    }
    let binding = envelope.binding;
    parts.extensions.insert(AuthenticatedRequest {
        signer,
        binding: binding.clone(),
    });
    parts.headers.insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );
    parts.headers.remove(axum::http::header::CONTENT_LENGTH);
    let response = next
        .run(Request::from_parts(parts, Body::from(envelope.body)))
        .await;
    let (mut parts, body) = response.into_parts();
    let bytes = body::to_bytes(body, MAX_BODY)
        .await
        .map_err(|_| ApiError::InternalServerError("Oversized response".into()))?;
    let mut response_binding = binding;
    response_binding.audience = response_binding.principal;
    response_binding.principal = state.config.coordinator_public_key.clone();
    response_binding.status = parts.status.as_u16();
    response_binding.timestamp = chrono::Utc::now().timestamp();
    let reply = Envelope::sign(response_binding, bytes.to_vec(), &state.response_key)?;
    parts.headers.remove(axum::http::header::CONTENT_LENGTH);
    Ok(Response::from_parts(
        parts,
        Body::from(serde_json::to_vec(&reply).map_err(anyhow::Error::from)?),
    ))
}
