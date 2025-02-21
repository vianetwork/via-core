use std::sync::Arc;

use axum::{
    body::{self, Body},
    extract::{Request, State},
    middleware::Next,
    response::Response,
};

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

    // Validate the verifier index
    if verifier_index >= state.state.verifiers_pub_keys.len() {
        return Err(ApiError::Unauthorized("Invalid verifier index".into()));
    }

    let timestamp_now = chrono::Utc::now().timestamp();
    let timestamp_diff = timestamp_now - timestamp.parse::<i64>().unwrap();

    //Todo: move this to config
    if timestamp_diff > 10 {
        return Err(ApiError::Unauthorized("Timestamp is too old".into()));
    }

    // Get the public key for this verifier
    let public_key = &state.state.verifiers_pub_keys[verifier_index];

    //  verify timestamp + verifier_index
    let payload = serde_json::json!({
        "timestamp": timestamp,
        "verifier_index": verifier_index.to_string(),
    });

    // Verify the signature
    if !crate::auth::verify_signature(&payload, signature, public_key)
        .map_err(|_| ApiError::InternalServerError("Signature verification failed".into()))?
    {
        return Err(ApiError::Unauthorized(
            "Invalid authentication signature".into(),
        ));
    }

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
