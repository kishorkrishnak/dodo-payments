use chrono::{Duration as ChronoDuration, Utc};
use hmac::{Hmac, Mac};
use reqwest::{redirect::Policy, Client};
use sha2::Sha256;
use sqlx::PgPool;
use std::time::Duration;
use tracing::{error, info, warn};
use uuid::Uuid;

// Webhook delivery is completely decoupled from the synchronous API request path using the
// Transactional Outbox Pattern:
// 1. Handlers write `webhook_events` and `webhook_deliveries` in the same atomic SQL transaction as the state change.
// 2. Workers atomically lease rows using `FOR UPDATE SKIP LOCKED`; an expired lease supports crash recovery.
// 3. Signing Scheme: HMAC-SHA256 over `${timestamp}.${payload}` in the `X-Webhook-Signature` header (t=...,v1=...)
//    for cryptographic authenticity and replay attack prevention.
// 4. Retry Backoff Policy:
//    - Max attempts: 5 retries (total 6 attempts including initial).
//    - Interval schedule: Attempt 1 (+30s), Attempt 2 (+2m), Attempt 3 (+10m), Attempt 4 (+1h), Attempt 5 (+6h).
//    - Total budget: ~7.2 hours.
//    - Terminal failure: Once budget is exhausted, delivery is marked 'failed'.
//    - Reconciliation: Merchants can poll `GET /v1/webhooks/events` to reconcile any dropped deliveries.

type HmacSha256 = Hmac<Sha256>;

pub struct WebhookWorker {
    pool: PgPool,
    http_client: Client,
}

#[derive(sqlx::FromRow)]
struct PendingDeliveryRow {
    delivery_id: Uuid,
    event_id: Uuid,
    endpoint_url: String,
    endpoint_secret: String,
    event_type: String,
    event_payload: serde_json::Value,
    attempts: i32,
}

impl WebhookWorker {
    pub fn new(pool: PgPool) -> Self {
        let http_client = Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(Policy::none())
            .build()
            .expect("Failed to create HTTP client for WebhookWorker");

        Self { pool, http_client }
    }

    pub async fn run_loop(self) {
        info!("Starting background webhook delivery worker...");
        let mut interval = tokio::time::interval(Duration::from_secs(2));

        loop {
            interval.tick().await;
            if let Err(err) = self.process_pending_deliveries().await {
                error!(error = %err, "Error in webhook worker processing loop");
            }
        }
    }

    async fn process_pending_deliveries(&self) -> Result<(), sqlx::Error> {
        // Claim deliveries with a lease so separate service replicas cannot deliver
        // one row concurrently. An expired lease makes crash recovery at-least-once.
        let rows = sqlx::query_as::<_, PendingDeliveryRow>(
            r#"
            WITH candidates AS (
                SELECT d.id
                FROM webhook_deliveries d
                JOIN webhook_endpoints e ON d.webhook_endpoint_id = e.id
                WHERE e.is_active = TRUE AND (
                    (d.status = 'pending' AND d.next_retry_at <= NOW())
                    OR (d.status = 'in_progress' AND d.lease_expires_at <= NOW())
                )
                ORDER BY d.next_retry_at ASC
                FOR UPDATE OF d SKIP LOCKED
                LIMIT 20
            ), claimed AS (
                UPDATE webhook_deliveries d
                SET status = 'in_progress', lease_expires_at = NOW() + INTERVAL '30 seconds'
                FROM candidates c
                WHERE d.id = c.id
                RETURNING d.id, d.webhook_event_id, d.webhook_endpoint_id, d.attempts
            )
            SELECT
                c.id AS delivery_id,
                c.webhook_event_id AS event_id,
                e.url AS endpoint_url,
                e.secret AS endpoint_secret,
                ev.event_type AS event_type,
                ev.payload AS event_payload,
                c.attempts AS attempts
            FROM claimed c
            JOIN webhook_endpoints e ON c.webhook_endpoint_id = e.id
            JOIN webhook_events ev ON c.webhook_event_id = ev.id
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        for item in rows {
            self.attempt_delivery(item).await;
        }

        Ok(())
    }

    async fn attempt_delivery(&self, item: PendingDeliveryRow) {
        let now_ts = Utc::now().timestamp();
        let payload_str = item.event_payload.to_string();

        // Calculate HMAC-SHA256 signature
        let signature_payload = format!("{}.{}", now_ts, payload_str);
        let mut mac = HmacSha256::new_from_slice(item.endpoint_secret.as_bytes())
            .expect("HMAC can take key of any size");
        mac.update(signature_payload.as_bytes());
        let signature_hex = hex::encode(mac.finalize().into_bytes());

        let signature_header = format!("t={},v1={}", now_ts, signature_hex);

        let res = self
            .http_client
            .post(&item.endpoint_url)
            .header("Content-Type", "application/json")
            .header("X-Webhook-Signature", signature_header)
            .header("X-Webhook-Event", &item.event_type)
            .header("X-Webhook-Event-Id", item.event_id.to_string())
            .body(payload_str)
            .send()
            .await;

        let new_attempts = item.attempts + 1;
        let is_success = match &res {
            Ok(resp) => resp.status().is_success(),
            Err(_) => false,
        };

        if is_success {
            let status_code = res.ok().map(|r| r.status().as_u16() as i32);
            info!(
                delivery_id = %item.delivery_id,
                endpoint = %item.endpoint_url,
                attempts = %new_attempts,
                "Webhook delivered successfully"
            );
            if let Err(err) = sqlx::query(
                r#"
                UPDATE webhook_deliveries
                SET status = 'delivered',
                    attempts = $1,
                    last_attempt_at = NOW(),
                    response_code = $2,
                    error_message = NULL,
                    lease_expires_at = NULL
                WHERE id = $3
                "#,
            )
            .bind(new_attempts)
            .bind(status_code)
            .bind(item.delivery_id)
            .execute(&self.pool)
            .await
            {
                error!(delivery_id = %item.delivery_id, error = %err, "Could not record successful webhook delivery");
            }
        } else {
            let (status_code, error_msg) = match res {
                Ok(resp) => (
                    Some(resp.status().as_u16() as i32),
                    Some(format!("HTTP status {}", resp.status())),
                ),
                Err(err) => (None, Some(err.to_string())),
            };

            // Exponential backoff schedule with 5 max retry attempts:
            // 1: +30s, 2: +120s, 3: +600s, 4: +3600s, 5: +21600s
            // If new_attempts > 5: mark status = 'failed' (terminal state).
            const MAX_RETRIES: i32 = 5;
            if new_attempts > MAX_RETRIES {
                warn!(
                    delivery_id = %item.delivery_id,
                    endpoint = %item.endpoint_url,
                    attempts = %new_attempts,
                    "Webhook exhausted retry budget; marking permanently failed"
                );
                if let Err(err) = sqlx::query(
                    r#"
                    UPDATE webhook_deliveries
                    SET status = 'failed',
                        attempts = $1,
                        last_attempt_at = NOW(),
                        response_code = $2,
                        error_message = $3,
                        lease_expires_at = NULL
                    WHERE id = $4
                    "#,
                )
                .bind(new_attempts)
                .bind(status_code)
                .bind(error_msg)
                .bind(item.delivery_id)
                .execute(&self.pool)
                .await
                {
                    error!(delivery_id = %item.delivery_id, error = %err, "Could not record terminal webhook failure");
                }
            } else {
                let backoff_secs = match new_attempts {
                    1 => 30,
                    2 => 120,
                    3 => 600,
                    4 => 3600,
                    _ => 21600,
                };
                let next_retry = Utc::now() + ChronoDuration::seconds(backoff_secs);
                warn!(
                    delivery_id = %item.delivery_id,
                    attempts = %new_attempts,
                    next_retry_in_secs = %backoff_secs,
                    "Webhook delivery failed; scheduled retry"
                );
                if let Err(err) = sqlx::query(
                    r#"
                    UPDATE webhook_deliveries
                    SET status = 'pending',
                        attempts = $1,
                        last_attempt_at = NOW(),
                        next_retry_at = $2,
                        response_code = $3,
                        error_message = $4,
                        lease_expires_at = NULL
                    WHERE id = $5
                    "#,
                )
                .bind(new_attempts)
                .bind(next_retry)
                .bind(status_code)
                .bind(error_msg)
                .bind(item.delivery_id)
                .execute(&self.pool)
                .await
                {
                    error!(delivery_id = %item.delivery_id, error = %err, "Could not schedule webhook retry");
                }
            }
        }
    }
}
