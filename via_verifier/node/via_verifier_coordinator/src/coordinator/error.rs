use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use thiserror::Error;
use tracing::error;
use via_verifier_dal::DalError;
use zksync_types::protocol_version::ParseProtocolSemanticVersionError;

// Custom error type for API-specific errors
#[derive(Error, Debug)]
pub enum ApiError {
    #[error("Invalid input: {0}")]
    BadRequest(String),
    #[error("Unauthorized: {0}")]
    Unauthorized(String),
    #[error("Unexpected error: {0}")]
    InternalServerError(String),
    #[error("New signing round exceeds supported transport capacity")]
    SigningRoundTooLarge,
    #[error("Retained signing round requires size reconciliation; holds remain active")]
    SigningRoundUnservable,
    #[error("Authenticated response exceeds supported transport capacity")]
    ResponseTooLarge,
    #[error("Authentication envelope metadata exceeds supported capacity")]
    RequestTooLarge,
    #[error("Invalid transport encoding: {0}")]
    InvalidTransportEncoding(String),
}

impl From<anyhow::Error> for ApiError {
    fn from(error: anyhow::Error) -> Self {
        ApiError::InternalServerError(error.to_string())
    }
}

impl From<DalError> for ApiError {
    fn from(error: DalError) -> Self {
        ApiError::InternalServerError(error.to_string())
    }
}

impl From<ParseProtocolSemanticVersionError> for ApiError {
    fn from(error: ParseProtocolSemanticVersionError) -> Self {
        ApiError::BadRequest(error.to_string())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, error_response) = match self {
            ApiError::InvalidTransportEncoding(ref message) => (
                StatusCode::BAD_REQUEST,
                ErrorResponse::coded("invalid_transport_encoding", message),
            ),
            ApiError::SigningRoundTooLarge => (
                StatusCode::PAYLOAD_TOO_LARGE,
                ErrorResponse::coded("signing_round_too_large", &self),
            ),
            ApiError::SigningRoundUnservable => (
                StatusCode::SERVICE_UNAVAILABLE,
                ErrorResponse::coded("signing_round_unservable", &self),
            ),
            ApiError::ResponseTooLarge => (
                StatusCode::SERVICE_UNAVAILABLE,
                ErrorResponse::coded("response_too_large", &self),
            ),
            ApiError::RequestTooLarge => (
                StatusCode::PAYLOAD_TOO_LARGE,
                ErrorResponse::coded("request_too_large", &self),
            ),
            ApiError::BadRequest(msg) => (StatusCode::BAD_REQUEST, ErrorResponse::new(&msg)),
            ApiError::Unauthorized(msg) => (StatusCode::UNAUTHORIZED, ErrorResponse::new(&msg)),
            ApiError::InternalServerError(msg) => {
                (StatusCode::INTERNAL_SERVER_ERROR, ErrorResponse::new(&msg))
            }
        };

        let response = Json(error_response).into_response();
        (status, response).into_response()
    }
}

// Struct for standardized error responses
#[derive(Serialize)]
struct ErrorResponse {
    error: String,
    message: String,
}

impl ErrorResponse {
    fn coded(code: &str, message: &impl std::fmt::Display) -> Self {
        Self { error: code.to_owned(), message: message.to_string() }
    }

    fn new<E: std::fmt::Display>(message: &E) -> Self {
        Self {
            error: "Coordinator API Error".to_string(),
            message: message.to_string(),
        }
    }
}
