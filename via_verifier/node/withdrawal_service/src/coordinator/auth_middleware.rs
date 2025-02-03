use std::sync::Arc;

use axum::{body::Bytes, extract::Request, middleware::Next, response::Response};
use serde_json::Value;

use crate::coordinator::{api_decl::RestApi, error::ApiError};

pub async fn auth_middleware(request: Request, next: Next) -> Result<Response, ApiError> {
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

    // Get state from request extensions
    let state = request
        .extensions()
        .get::<Arc<RestApi>>()
        .ok_or_else(|| ApiError::InternalServerError("Missing state".into()))?;

    // Validate verifier index
    if verifier_index >= state.state.verifiers_pub_keys.len() {
        return Err(ApiError::Unauthorized("Invalid verifier index".into()));
    }

    // Get the public key for this verifier
    let public_key = &state.state.verifiers_pub_keys[verifier_index];

    // Create verification payload based on request method and body
    let payload = if request.method().is_safe() {
        // For GET requests, verify timestamp + verifier_index
        serde_json::json!({
            "timestamp": timestamp,
            "verifier_index": verifier_index.to_string(),
        })
    } else {
        // For POST/PUT requests, verify the body
        let body_bytes = request
            .extensions()
            .get::<Bytes>()
            .ok_or_else(|| ApiError::InternalServerError("Missing request body".into()))?;

        serde_json::from_slice::<Value>(body_bytes)
            .map_err(|_| ApiError::BadRequest("Invalid JSON body".into()))?
    };

    // Verify the signature
    if !crate::auth::verify_signature(&payload, signature, public_key)
        .map_err(|_| ApiError::InternalServerError("Signature verification failed".into()))?
    {
        return Err(ApiError::Unauthorized("Invalid signature".into()));
    }

    Ok(next.run(request).await)
}

// Helper function to extract body as bytes
pub async fn extract_body(request: Request, next: Next) -> Result<Response, ApiError> {
    let (parts, body) = request.into_parts();
    let bytes = axum::body::to_bytes(body, usize::MAX)
        .await
        .map_err(|_| ApiError::InternalServerError("Failed to read body".into()))?;

    let mut request = Request::from_parts(parts, axum::body::Body::empty());
    request.extensions_mut().insert(bytes);

    Ok(next.run(request).await)
}
