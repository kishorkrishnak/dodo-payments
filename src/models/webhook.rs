use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct WebhookEndpoint {
    pub id: Uuid,
    pub business_id: Uuid,
    pub url: String,
    pub secret: String,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct RegisterWebhookRequest {
    pub url: String,
}

#[derive(Debug, Serialize)]
pub struct WebhookEndpointResponse {
    pub id: Uuid,
    pub business_id: Uuid,
    pub url: String,
    pub secret: String,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct WebhookEvent {
    pub id: Uuid,
    pub business_id: Uuid,
    pub event_type: String,
    pub payload: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct WebhookDelivery {
    pub id: Uuid,
    pub webhook_event_id: Uuid,
    pub webhook_endpoint_id: Uuid,
    pub status: String,
    pub attempts: i32,
    pub last_attempt_at: Option<DateTime<Utc>>,
    pub next_retry_at: DateTime<Utc>,
    pub response_code: Option<i32>,
    pub error_message: Option<String>,
    pub created_at: DateTime<Utc>,
}
