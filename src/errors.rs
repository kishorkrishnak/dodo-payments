use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::error;

#[derive(Debug, Serialize, Deserialize)]
pub struct ErrorDetail {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ApiErrorResponse {
    pub error: ErrorDetail,
}

#[derive(Error, Debug)]
pub enum AppError {
    #[error("Authentication required or invalid API key")]
    Unauthorized,

    #[error("Resource not found: {0}")]
    NotFound(String),

    #[error("Invalid state transition: {0}")]
    InvalidStateTransition(String),

    #[error("Idempotency conflict: {0}")]
    IdempotencyConflict(String),

    #[error("Payment failed: {0}")]
    PaymentFailed(String),

    #[error("Payment processor timed out: {0}")]
    PspTimeout(String),

    #[error("Payment processor network failure: {0}")]
    PspNetworkError(String),

    #[error("Bad request: {0}")]
    BadRequest(String),

    #[error("Idempotency-Key header is required for payment requests")]
    IdempotencyKeyRequired,

    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("Internal server error")]
    Internal(String),
}

impl AppError {
    pub fn status_code(&self) -> StatusCode {
        match self {
            AppError::Unauthorized => StatusCode::UNAUTHORIZED,
            AppError::NotFound(_) => StatusCode::NOT_FOUND,
            AppError::InvalidStateTransition(_) => StatusCode::UNPROCESSABLE_ENTITY,
            AppError::IdempotencyConflict(_) => StatusCode::CONFLICT,
            AppError::PaymentFailed(_) => StatusCode::PAYMENT_REQUIRED,
            AppError::PspTimeout(_) => StatusCode::GATEWAY_TIMEOUT,
            AppError::PspNetworkError(_) => StatusCode::BAD_GATEWAY,
            AppError::BadRequest(_) => StatusCode::BAD_REQUEST,
            AppError::IdempotencyKeyRequired => StatusCode::BAD_REQUEST,
            AppError::Database(_) => StatusCode::INTERNAL_SERVER_ERROR,
            AppError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    pub fn error_code(&self) -> &'static str {
        match self {
            AppError::Unauthorized => "unauthorized",
            AppError::NotFound(_) => "not_found",
            AppError::InvalidStateTransition(_) => "invalid_state_transition",
            AppError::IdempotencyConflict(_) => "idempotency_conflict",
            AppError::PaymentFailed(_) => "payment_failed",
            AppError::PspTimeout(_) => "psp_timeout",
            AppError::PspNetworkError(_) => "psp_network_error",
            AppError::BadRequest(_) => "bad_request",
            AppError::IdempotencyKeyRequired => "idempotency_key_required",
            AppError::Database(_) => "database_error",
            AppError::Internal(_) => "internal_server_error",
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status_code();
        let is_internal = matches!(self, AppError::Database(_) | AppError::Internal(_));
        if is_internal {
            error!(error = %self, "Unhandled internal application error");
        }
        let body = ApiErrorResponse {
            error: ErrorDetail {
                code: self.error_code().to_string(),
                message: if is_internal {
                    "Internal server error".to_string()
                } else {
                    self.to_string()
                },
            },
        };
        (status, Json(body)).into_response()
    }
}
