use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::{error, info, warn};

// Downstream calls have a bounded timeout. A client timeout or transport failure is
// ambiguous and is handled as an unknown outcome; the supplied mock's explicit 500
// is a retryable failed attempt. These cases are intentionally distinct.

#[derive(Debug, Serialize)]
struct PspChargePayload<'a> {
    amount_cents: i64,
    currency: &'a str,
    token: &'a str,
    idempotency_key: Option<&'a str>,
}

#[derive(Debug, Deserialize)]
struct PspSuccessBody {
    status: String,
    psp_ref: String,
}

#[derive(Debug, Deserialize)]
struct PspFailBody {
    status: Option<String>,
    code: Option<String>,
    error: Option<String>,
}

#[derive(Debug)]
pub enum PspChargeResult {
    Success { psp_ref: String },
    Declined { code: String },
    Timeout,
    InProgress,
    RetryableFailure { message: String },
    NetworkError { message: String },
}

#[derive(Clone)]
pub struct PspClient {
    client: Client,
    base_url: String,
}

impl PspClient {
    pub fn new(base_url: String, timeout_secs: u64) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .build()
            .expect("Failed to build reqwest HTTP client for PSP");

        Self { client, base_url }
    }

    pub async fn charge(
        &self,
        amount_cents: i64,
        currency: &str,
        token: &str,
        idempotency_key: Option<&str>,
    ) -> PspChargeResult {
        let url = format!("{}/charges", self.base_url.trim_end_matches('/'));
        let payload = PspChargePayload {
            amount_cents,
            currency,
            token,
            idempotency_key,
        };

        info!(
            target_url = %url,
            amount_cents = %amount_cents,
            "Dispatching charge to external PSP"
        );

        let response = match self.client.post(&url).json(&payload).send().await {
            Ok(res) => res,
            Err(err) => {
                if err.is_timeout() {
                    warn!("PSP request timed out after timeout limit");
                    return PspChargeResult::Timeout;
                } else {
                    // Connection/DNS failures have no reliable processor outcome.
                    error!(
                        error = %err,
                        "PSP network level connection failure"
                    );
                    return PspChargeResult::NetworkError {
                        message: err.to_string(),
                    };
                }
            }
        };

        let status = response.status();
        if status.is_success() {
            match response.json::<PspSuccessBody>().await {
                Ok(body) if body.status == "succeeded" => {
                    info!(
                        psp_ref = %body.psp_ref,
                        "PSP charge succeeded"
                    );
                    PspChargeResult::Success {
                        psp_ref: body.psp_ref,
                    }
                }
                Ok(_) => PspChargeResult::NetworkError {
                    message: "PSP returned a successful HTTP status with an invalid payment status"
                        .to_string(),
                },
                Err(err) => {
                    error!(error = %err, "Failed to parse PSP success response body");
                    PspChargeResult::NetworkError {
                        message: "Invalid response JSON from PSP".to_string(),
                    }
                }
            }
        } else if status.is_client_error() {
            // e.g. 400 Bad Request, 402 Payment Required (insufficient_funds, card_declined)
            match response.json::<PspFailBody>().await {
                Ok(body) => {
                    if body.error.as_deref() == Some("idempotency_in_progress") {
                        return PspChargeResult::InProgress;
                    }
                    let code = body
                        .code
                        .or(body.status)
                        .unwrap_or_else(|| "declined".to_string());
                    warn!(code = %code, "PSP payment declined");
                    PspChargeResult::Declined { code }
                }
                Err(_) => PspChargeResult::Declined {
                    code: "card_declined".to_string(),
                },
            }
        } else {
            // 5xx server errors from PSP
            error!(
                status = %status,
                "PSP returned 5xx server error"
            );
            PspChargeResult::RetryableFailure {
                message: format!("PSP returned server error status: {}", status),
            }
        }
    }
}
