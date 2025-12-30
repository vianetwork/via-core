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

    let parsed_timestamp = validate_timestamp(timestamp).map_err(|msg| {
        tracing::warn!(
            "Rejected invalid timestamp '{}' from verifier {}: {}",
            timestamp,
            verifier_index,
            msg
        );
        ApiError::Unauthorized(msg.into())
    })?;

    let timestamp_diff = timestamp_now - parsed_timestamp;

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

/// Validates and parses a timestamp string.
/// Returns the parsed timestamp if valid, or an error description if invalid.
pub fn validate_timestamp(timestamp: &str) -> Result<i64, &'static str> {
    // Validate the format: a non-empty sequence of ASCII digits, optionally prefixed with a single '-'.
    let is_valid_format = if let Some(rest) = timestamp.strip_prefix('-') {
        !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit())
    } else {
        !timestamp.is_empty() && timestamp.chars().all(|c| c.is_ascii_digit())
    };

    if !is_valid_format {
        return Err("Invalid timestamp format");
    }

    let parsed = timestamp.parse::<i64>().map_err(|_| "Invalid timestamp")?;

    // Bounds checking
    const MAX_TIMESTAMP: i64 = 253402300799; // Year 9999
    const MIN_TIMESTAMP: i64 = 0; // Unix epoch

    if parsed < MIN_TIMESTAMP || parsed > MAX_TIMESTAMP {
        return Err("Timestamp out of valid range");
    }

    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_malformed_timestamp_returns_error() {
        // Test with "not-a-number"
        assert!(validate_timestamp("not-a-number").is_err());
        assert!(validate_timestamp("abc").is_err());
        assert!(validate_timestamp("12abc34").is_err());

        // Test with empty string
        assert!(validate_timestamp("").is_err());

        // Test with special characters
        assert!(validate_timestamp("123!@#").is_err());
        assert!(validate_timestamp("12.34").is_err());
        assert!(validate_timestamp("12 34").is_err());
        assert!(validate_timestamp("123\n456").is_err());
        assert!(validate_timestamp("123\t456").is_err());

        // Test with overflow values (beyond i64::MAX)
        assert!(validate_timestamp("99999999999999999999999999").is_err());
        assert!(validate_timestamp("9223372036854775808").is_err()); // i64::MAX + 1
    }

    #[test]
    fn test_valid_timestamp_parses_correctly() {
        // Test with a valid Unix timestamp (current time range)
        let now = chrono::Utc::now().timestamp();
        assert_eq!(validate_timestamp(&now.to_string()).unwrap(), now);

        // Test with specific valid timestamps
        assert_eq!(validate_timestamp("1704067200").unwrap(), 1704067200); // 2024-01-01 00:00:00 UTC
        assert_eq!(validate_timestamp("0").unwrap(), 0); // Unix epoch
        assert_eq!(validate_timestamp("1").unwrap(), 1); // One second after epoch
        assert_eq!(validate_timestamp("1609459200").unwrap(), 1609459200); // 2021-01-01

        // Test with leading zeros (valid, parses correctly)
        assert_eq!(validate_timestamp("0000001704067200").unwrap(), 1704067200);
    }

    #[test]
    fn test_negative_timestamp_handled() {
        // Test with negative timestamp (before epoch) - should be rejected per bound check
        assert!(validate_timestamp("-1").is_err());
        assert!(validate_timestamp("-1000000000").is_err());
        assert!(validate_timestamp("-9223372036854775808").is_err()); // i64::MIN
    }

    #[test]
    fn test_future_timestamp_handled() {
        // Test with a far-future timestamp (within bounds)
        assert!(validate_timestamp("253402300799").is_ok()); // Year 9999 - max valid
        assert_eq!(
            validate_timestamp("253402300799").unwrap(),
            253402300799
        );

        // Test with timestamp beyond max
        assert!(validate_timestamp("253402300800").is_err()); // Beyond year 9999
        assert!(validate_timestamp("300000000000").is_err()); // Way beyond
    }

    #[test]
    fn test_edge_cases() {
        // Just a minus sign
        assert!(validate_timestamp("-").is_err());

        // Multiple minus signs
        assert!(validate_timestamp("--123").is_err());
        assert!(validate_timestamp("123-456").is_err());
        assert!(validate_timestamp("---").is_err());

        // Whitespace
        assert!(validate_timestamp(" 123").is_err());
        assert!(validate_timestamp("123 ").is_err());
        assert!(validate_timestamp(" ").is_err());

        // Unicode digits (should be rejected - only ASCII)
        assert!(validate_timestamp("١٢٣").is_err()); // Arabic-Indic digits

        // Boundary values
        assert!(validate_timestamp("9223372036854775807").is_err()); // i64::MAX - beyond year 9999
    }

    #[test]
    fn test_timestamp_diff_calculation() {
        // Verify the timestamp difference calculation doesn't overflow
        let timestamp_now = chrono::Utc::now().timestamp();
        let valid_past = validate_timestamp(&(timestamp_now - 60).to_string()).unwrap();
        let diff = timestamp_now - valid_past;
        assert_eq!(diff, 60);

        // Test with timestamp at epoch
        let epoch = validate_timestamp("0").unwrap();
        let diff_from_epoch = timestamp_now - epoch;
        assert!(diff_from_epoch > 0);
    }
}
