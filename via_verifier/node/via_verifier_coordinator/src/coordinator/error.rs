use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use thiserror::Error;
use tracing::error;
use via_verifier_dal::DalError;

// Custom error type for API-specific errors
#[derive(Error, Debug)]
pub enum ApiError {
    #[error("Invalid input: {0}")]
    BadRequest(String),
    #[error("Unauthorized: {0}")]
    Unauthorized(String),
    #[error("Unexpected error: {0}")]
    InternalServerError(String),
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

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, error_response) = match self {
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
    fn new<E: std::fmt::Display>(message: &E) -> Self {
        Self {
            error: "Coordinator API Error".to_string(),
            message: message.to_string(),
        }
    }
}
