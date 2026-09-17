use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "VARCHAR", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum PaymentAttemptStatus {
    Pending,
    Unknown,
    Succeeded,
    Failed,
}

impl PaymentAttemptStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            PaymentAttemptStatus::Pending => "pending",
            PaymentAttemptStatus::Unknown => "unknown",
            PaymentAttemptStatus::Succeeded => "succeeded",
            PaymentAttemptStatus::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct PaymentAttempt {
    pub id: Uuid,
    pub invoice_id: Uuid,
    pub idempotency_key: Option<String>,
    pub amount_cents: i64,
    pub status: String,
    pub payment_method_reference: String,
    pub psp_reference: Option<String>,
    pub error_code: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PayInvoiceRequest {
    pub token: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PayInvoiceResponse {
    pub invoice_id: Uuid,
    pub payment_attempt_id: Uuid,
    pub status: String,
    pub invoice_status: String,
    pub psp_reference: Option<String>,
    pub error_code: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct IdempotencyRecord {
    pub id: Uuid,
    pub business_id: Uuid,
    pub idempotency_key: String,
    pub request_path: String,
    pub request_hash: String,
    pub response_status_code: Option<i32>,
    pub response_body: Option<String>,
    pub status: String,
    pub payment_attempt_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
