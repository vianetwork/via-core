use std::{str::FromStr, sync::Arc};

use axum::{
    body::{self, Body},
    extract::{Request, State},
    middleware::Next,
    response::Response,
};
use via_verifier_dal::VerifierDal;
use zksync_types::protocol_version::ProtocolSemanticVersion;

use crate::coordinator::{api_decl::RestApi, error::ApiError};

pub async fn auth_middleware(
    State(state): State<Arc<RestApi>>,
    request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let headers = request.headers();

    // Extract required headers
    let timestamp = headers
        .get("X-Timestamp")
        .and_then(|h| h.to_str().ok())
        .ok_or_else(|| ApiError::Unauthorized("Missing timestamp header".into()))?;

    let verifier_index = headers
        .get("X-Verifier-Index")
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.parse::<usize>().ok())
        .ok_or_else(|| ApiError::Unauthorized("Missing or invalid verifier index".into()))?;

    let signature = headers
        .get("X-Signature")
        .and_then(|h| h.to_str().ok())
        .ok_or_else(|| ApiError::Unauthorized("Missing signature header".into()))?;

    let sequencer_version = headers
        .get("X-Sequencer-Version")
        .and_then(|h| h.to_str().ok())
        .ok_or_else(|| ApiError::Unauthorized("Missing sequencer version header".into()))?;

    // Validate the verifier index
    if verifier_index >= state.state.verifiers_pub_keys.len() {
        return Err(ApiError::Unauthorized("Invalid verifier index".into()));
    }

    let timestamp_now = chrono::Utc::now().timestamp();
    let timestamp_diff = timestamp_now - timestamp.parse::<i64>().unwrap();

    if timestamp_diff > state.state.verifier_request_timeout.into() {
        return Err(ApiError::Unauthorized("Timestamp is too old".into()));
    }

    // Get the public key for this verifier
    let public_key = &state.state.verifiers_pub_keys[verifier_index];

    //  verify timestamp + verifier_index
    let payload = serde_json::json!({
        "timestamp": timestamp,
        "verifier_index": verifier_index.to_string(),
        "sequencer_version": sequencer_version
    });

    // Verify the signature
    if !crate::auth::verify_signature(&payload, signature, public_key)
        .map_err(|_| ApiError::InternalServerError("Signature verification failed".into()))?
    {
        return Err(ApiError::Unauthorized(
            "Invalid authentication signature".into(),
        ));
    }

    // Check the protocol version after the signature validation to make sure the caller is legit and avoid access db
    let mut storage = state.master_connection_pool.connection().await?;

    if let Some(latest_protocol_semantic_version) = storage
        .via_protocol_versions_dal()
        .latest_protocol_semantic_version()
        .await
        .expect("Error load the protocol version")
    {
        if ProtocolSemanticVersion::from_str(sequencer_version)? < latest_protocol_semantic_version
        {
            return Err(ApiError::Unauthorized(
                "Invalid verifier protocol version".into(),
            ));
        }
    }

    drop(storage);

    Ok(next.run(request).await)
}

pub async fn extract_body(
    State(_state): State<Arc<RestApi>>,
    request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let (parts, body) = request.into_parts();
    let bytes = body::to_bytes(body, usize::MAX)
        .await
        .map_err(|_| ApiError::InternalServerError("Failed to read body".into()))?;
    let mut req = Request::from_parts(parts, Body::from(bytes.clone()));
    req.extensions_mut().insert(bytes);
    Ok(next.run(req).await)
}
