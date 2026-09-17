use axum::{
    extract::Json,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::time::Duration;
use std::{collections::HashMap, sync::Arc};
use tokio::sync::Mutex;
use tracing::{info, warn};
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct ChargeRequest {
    pub amount_cents: i64,
    pub currency: String,
    pub token: String,
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ChargeSuccessResponse {
    pub status: String,
    pub psp_ref: String,
}

#[derive(Debug, Serialize)]
pub struct ChargeFailedResponse {
    pub status: String,
    pub code: String,
}

#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub error: String,
}

#[derive(Clone)]
struct MockPspState {
    // The real PSP contract is idempotent. This in-memory map is sufficient for this
    // assignment's mock process; a real processor persists this before charging.
    outcomes: Arc<Mutex<HashMap<String, MockChargeState>>>,
}

#[derive(Clone)]
enum MockChargeState {
    InProgress,
    Completed(serde_json::Value),
}

async fn health_check() -> &'static str {
    "Mock PSP is healthy"
}

async fn process_charge(
    axum::extract::State(state): axum::extract::State<MockPspState>,
    Json(req): Json<ChargeRequest>,
) -> impl IntoResponse {
    info!(
        amount_cents = %req.amount_cents,
        "Received PSP charge request"
    );

    let idempotent_charge = matches!(req.token.as_str(), "tok_success" | "tok_timeout");
    if idempotent_charge {
        if let Some(key) = &req.idempotency_key {
            let mut outcomes = state.outcomes.lock().await;
            match outcomes.get(key) {
                Some(MockChargeState::Completed(outcome)) => {
                    return (StatusCode::OK, Json(outcome.clone()));
                }
                Some(MockChargeState::InProgress) => {
                    let body = serde_json::to_value(ErrorResponse {
                        error: "idempotency_in_progress".to_string(),
                    })
                    .expect("mock PSP response serializes");
                    return (StatusCode::CONFLICT, Json(body));
                }
                None => {
                    outcomes.insert(key.clone(), MockChargeState::InProgress);
                }
            }
        }
    }

    match req.token.as_str() {
        "tok_success" => {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let resp = ChargeSuccessResponse {
                status: "succeeded".to_string(),
                psp_ref: Uuid::new_v4().to_string(),
            };
            let outcome = serde_json::to_value(resp).expect("mock PSP response serializes");
            if let Some(key) = req.idempotency_key {
                state
                    .outcomes
                    .lock()
                    .await
                    .insert(key, MockChargeState::Completed(outcome.clone()));
            }
            (StatusCode::OK, Json(outcome))
        }
        "tok_insufficient_funds" => {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let resp = ChargeFailedResponse {
                status: "failed".to_string(),
                code: "insufficient_funds".to_string(),
            };
            (
                StatusCode::PAYMENT_REQUIRED,
                Json(serde_json::to_value(resp).expect("mock PSP response serializes")),
            )
        }
        "tok_card_declined" => {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let resp = ChargeFailedResponse {
                status: "failed".to_string(),
                code: "card_declined".to_string(),
            };
            (
                StatusCode::PAYMENT_REQUIRED,
                Json(serde_json::to_value(resp).expect("mock PSP response serializes")),
            )
        }
        "tok_timeout" => {
            warn!("tok_timeout received: sleeping for 30 seconds to simulate upstream gateway timeout");
            let resp = ChargeSuccessResponse {
                status: "succeeded".to_string(),
                psp_ref: Uuid::new_v4().to_string(),
            };
            let outcome = serde_json::to_value(resp).expect("mock PSP response serializes");
            if let Some(key) = req.idempotency_key {
                let outcomes = state.outcomes.clone();
                let eventual_outcome = outcome.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    outcomes
                        .lock()
                        .await
                        .insert(key, MockChargeState::Completed(eventual_outcome));
                });
            }
            tokio::time::sleep(Duration::from_secs(30)).await;
            (StatusCode::OK, Json(outcome))
        }
        "tok_network_error" => {
            warn!("tok_network_error received: returning HTTP 500 network failure");
            let resp = ErrorResponse {
                error: "Internal network failure in mock PSP upstream gateway".to_string(),
            };
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::to_value(resp).expect("mock PSP response serializes")),
            )
        }
        _ => {
            let resp = ChargeFailedResponse {
                status: "failed".to_string(),
                code: "invalid_card_token".to_string(),
            };
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::to_value(resp).expect("mock PSP response serializes")),
            )
        }
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let app = Router::new()
        .route("/health", get(health_check))
        .route("/charges", post(process_charge))
        .with_state(MockPspState {
            outcomes: Arc::new(Mutex::new(HashMap::new())),
        });

    let port = std::env::var("PORT")
        .or_else(|_| std::env::var("PSP_PORT"))
        .unwrap_or_else(|_| "8081".to_string());
    let addr: SocketAddr = format!("0.0.0.0:{}", port).parse().unwrap();

    info!("Mock PSP listening on {}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
