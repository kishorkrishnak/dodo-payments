use axum::{extract::Json, http::StatusCode, routing::post, Router};
use serde::{Deserialize, Serialize};
use sqlx::{postgres::PgPoolOptions, PgPool};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

use dodo_payments::config::AppConfig;
use dodo_payments::models::business::Business;
use dodo_payments::models::customer::CustomerResponse;
use dodo_payments::services::invoice_service::InvoiceService;
use dodo_payments::services::psp_client::PspClient;

#[derive(Debug, Deserialize)]
pub struct MockChargeRequest {
    pub amount_cents: i64,
    pub currency: String,
    pub token: String,
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct MockChargeSuccess {
    pub status: String,
    pub psp_ref: String,
}

#[derive(Debug, Serialize)]
pub struct MockChargeFailed {
    pub status: String,
    pub code: String,
}

// In-process mock PSP router for fully self-contained integration tests
pub async fn start_mock_psp(call_counter: Arc<AtomicUsize>) -> String {
    let counter_clone = call_counter.clone();
    let app = Router::new().route(
        "/charges",
        post(move |Json(req): Json<MockChargeRequest>| {
            let counter = counter_clone.clone();
            async move {
                assert!(req.amount_cents >= 0);
                assert_eq!(req.currency, "USD");
                assert!(req.idempotency_key.is_some());
                counter.fetch_add(1, Ordering::SeqCst);
                match req.token.as_str() {
                    "tok_success" => {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        let resp = MockChargeSuccess {
                            status: "succeeded".to_string(),
                            psp_ref: Uuid::new_v4().to_string(),
                        };
                        (StatusCode::OK, Json(serde_json::to_value(resp).unwrap()))
                    }
                    "tok_insufficient_funds" => {
                        let resp = MockChargeFailed {
                            status: "failed".to_string(),
                            code: "insufficient_funds".to_string(),
                        };
                        (
                            StatusCode::PAYMENT_REQUIRED,
                            Json(serde_json::to_value(resp).unwrap()),
                        )
                    }
                    "tok_card_declined" => {
                        let resp = MockChargeFailed {
                            status: "failed".to_string(),
                            code: "card_declined".to_string(),
                        };
                        (
                            StatusCode::PAYMENT_REQUIRED,
                            Json(serde_json::to_value(resp).unwrap()),
                        )
                    }
                    "tok_timeout" => {
                        // Sleep longer than the client timeout to simulate gateway timeout
                        tokio::time::sleep(Duration::from_secs(3)).await;
                        let resp = MockChargeSuccess {
                            status: "succeeded".to_string(),
                            psp_ref: Uuid::new_v4().to_string(),
                        };
                        (StatusCode::OK, Json(serde_json::to_value(resp).unwrap()))
                    }
                    "tok_network_error" => (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(serde_json::json!({ "error": "PSP network failure" })),
                    ),
                    _ => (
                        StatusCode::BAD_REQUEST,
                        Json(serde_json::json!({ "error": "Invalid token" })),
                    ),
                }
            }
        }),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    format!("http://{}", addr)
}

pub async fn setup_test_context() -> (
    PgPool,
    Arc<InvoiceService>,
    Arc<AtomicUsize>,
    Business,
    CustomerResponse,
) {
    let config = AppConfig::from_env();
    let pool = PgPoolOptions::new()
        // Match the application pool. The concurrency test deliberately creates ten
        // simultaneous requests; reserve headroom for idempotency claims while
        // other requests hold invoice-lock transactions during the PSP call.
        .max_connections(20)
        .acquire_timeout(Duration::from_secs(2))
        .connect(&config.database_url)
        .await
        .expect("DATABASE_URL must point to PostgreSQL for required integration tests");

    // Run migrations
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("test database migrations must succeed");

    let psp_call_counter = Arc::new(AtomicUsize::new(0));
    let mock_psp_url = start_mock_psp(psp_call_counter.clone()).await;

    // Use a 1-second timeout for tests so tok_timeout (3s) triggers timeout promptly
    let psp_client = PspClient::new(mock_psp_url, 1);
    let service = Arc::new(InvoiceService::new(pool.clone(), psp_client));

    // Create unique test business
    let biz_id = Uuid::new_v4();
    let raw_key = format!("dp_test_{}", Uuid::new_v4().simple());
    let key_hash = Business::hash_api_key(&raw_key);

    let business = sqlx::query_as::<_, Business>(
        r#"
        INSERT INTO businesses (id, name, api_key_prefix, api_key_hash)
        VALUES ($1, $2, 'dp_test', $3)
        RETURNING *
        "#,
    )
    .bind(biz_id)
    .bind(format!("Test Biz {}", biz_id))
    .bind(key_hash)
    .fetch_one(&pool)
    .await
    .unwrap();

    // Create test customer
    let cust_id = Uuid::new_v4();
    let customer = sqlx::query_as::<_, dodo_payments::models::customer::Customer>(
        r#"
        INSERT INTO customers (id, business_id, name, email)
        VALUES ($1, $2, 'Test Customer', $3)
        RETURNING *
        "#,
    )
    .bind(cust_id)
    .bind(business.id)
    .bind(format!("cust_{}@example.com", cust_id))
    .fetch_one(&pool)
    .await
    .unwrap();

    (
        pool,
        service,
        psp_call_counter,
        business,
        CustomerResponse::from(customer),
    )
}
