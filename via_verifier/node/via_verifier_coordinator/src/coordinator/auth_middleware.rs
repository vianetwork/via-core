use std::{str::FromStr, sync::Arc};

use axum::{
    body::{self, Body},
    extract::{OriginalUri, Request, State},
    middleware::Next,
    response::{IntoResponse, Response},
};
use via_verifier_dal::VerifierDal;

use crate::{
    auth::{Binding, Envelope, MAX_BODY},
    coordinator::{api_decl::RestApi, error::ApiError},
    metrics::{SigningSizeLabel, SigningSizeStage, METRICS},
    transport,
};

/// Carry the verified key's signer slot to handlers, never a caller-asserted slot.
/// Matrix Synapse similarly propagates an authenticated origin; its server-name
/// key discovery is not used for Via's configured verifier set:
/// https://github.com/element-hq/synapse/blob/fcffd2e897aaef1583bcb1f93893f254330ec81c/synapse/federation/transport/server/_base.py
#[derive(Clone)]
pub struct AuthenticatedRequest {
    pub signer: usize,
    pub binding: Binding,
    pub transport: transport::TransportVersion,
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
    if bytes.is_empty() {
        return Err(ApiError::Unauthorized("Missing authenticated envelope".into()));
    }
    let (envelope, _) = transport::decode_envelope(&bytes)
        .map_err(|error| ApiError::InvalidTransportEncoding(error.to_string()))?;
    transport::check_metadata(&envelope.binding, &state.config.coordinator_public_key)
        .map_err(|_| ApiError::RequestTooLarge)?;
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
    let selected = transport::TransportVersion::negotiate(
        &target.parse::<axum::http::Uri>().map_err(anyhow::Error::from)?,
        state.config.withdrawal_compact_transport_writes_enabled,
    );
    let response_version = selected.as_ref().copied().unwrap_or_default();
    parts.extensions.insert(AuthenticatedRequest {
        signer,
        binding: binding.clone(),
        transport: response_version,
    });
    parts.headers.insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );
    parts.headers.remove(axum::http::header::CONTENT_LENGTH);
    let response = match selected {
        Ok(_) => next.run(Request::from_parts(parts, Body::from(envelope.body))).await,
        Err(error) => ApiError::InvalidTransportEncoding(error.to_string()).into_response(),
    };
    let (mut parts, body) = response.into_parts();
    let encode = |status: u16, body: Vec<u8>| -> anyhow::Result<Vec<u8>> {
        let response_binding = transport::response_binding(
            &binding,
            &state.config.coordinator_public_key,
            status,
            chrono::Utc::now().timestamp(),
        );
        let reply = Envelope::sign(response_binding, body, &state.response_key)?;
        Ok(transport::encode_envelope(reply, response_version, MAX_BODY)?)
    };
    let encoded = match body::to_bytes(body, MAX_BODY).await {
        Ok(bytes) => encode(parts.status.as_u16(), bytes.to_vec()),
        Err(error) => Err(error.into()),
    };
    let bytes = match encoded {
        Ok(bytes) => bytes,
        Err(error) => {
            METRICS.signing_size_refusals[&SigningSizeLabel { stage: SigningSizeStage::Response }]
                .inc();
            tracing::warn!("Authenticated reply refused: {error:#}");
            parts.status = axum::http::StatusCode::SERVICE_UNAVAILABLE;
            let fallback =
                body::to_bytes(ApiError::ResponseTooLarge.into_response().into_body(), MAX_BODY)
                    .await
                    .map_err(anyhow::Error::from)?
                    .to_vec();
            encode(parts.status.as_u16(), fallback)?
        }
    };
    parts.headers.remove(axum::http::header::CONTENT_LENGTH);
    Ok(Response::from_parts(parts, Body::from(bytes)))
}
