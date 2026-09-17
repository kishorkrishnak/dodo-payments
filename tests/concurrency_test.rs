mod common;

use axum::{
    body::Body,
    extract::FromRef,
    http::{Request, StatusCode},
    middleware,
    routing::post,
    Router,
};
use chrono::Utc;
use sqlx::PgPool;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;

use dodo_payments::models::business::Business;
use dodo_payments::models::invoice::{CreateInvoiceRequest, CreateLineItemRequest};
use dodo_payments::routes::{auth::require_api_key, invoices::pay_invoice};
use dodo_payments::services::invoice_service::InvoiceService;

#[derive(Clone)]
struct HttpTestState {
    pool: PgPool,
    invoice_service: Arc<InvoiceService>,
}

impl FromRef<HttpTestState> for PgPool {
    fn from_ref(state: &HttpTestState) -> Self {
        state.pool.clone()
    }
}

impl FromRef<HttpTestState> for Arc<InvoiceService> {
    fn from_ref(state: &HttpTestState) -> Self {
        state.invoice_service.clone()
    }
}

fn payment_router(state: HttpTestState) -> Router {
    let api_routes = Router::new()
        .route("/invoices/{id}/pay", post(pay_invoice))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            require_api_key,
        ));

    Router::new().nest("/v1", api_routes).with_state(state)
}

// REQUIRED TEST 1: HTTP-level acceptance coverage for the exact take-home requirement:
// POST /pay requests must pass through routing, authentication, header parsing,
// JSON parsing, the service, PostgreSQL, and the mock PSP.
#[tokio::test]
async fn test_concurrent_payment_posts_no_double_charge() {
    let (pool, service, psp_counter, business, customer) = common::setup_test_context().await;
    let api_key = format!("dp_http_test_{}", Uuid::new_v4().simple());
    sqlx::query("UPDATE businesses SET api_key_hash = $1 WHERE id = $2")
        .bind(Business::hash_api_key(&api_key))
        .bind(business.id)
        .execute(&pool)
        .await
        .unwrap();

    let invoice = service
        .create_invoice(
            business.id,
            CreateInvoiceRequest {
                customer_id: customer.id,
                due_date: Utc::now() + chrono::Duration::days(7),
                line_items: vec![CreateLineItemRequest {
                    description: "HTTP concurrency test".to_string(),
                    quantity: 1,
                    unit_amount_cents: 5000,
                }],
                auto_open: Some(true),
            },
        )
        .await
        .unwrap();

    let app = payment_router(HttpTestState {
        pool: pool.clone(),
        invoice_service: service,
    });
    const N: usize = 10;
    let mut calls = Vec::with_capacity(N);

    for i in 0..N {
        let app = app.clone();
        let api_key = api_key.clone();
        let request = Request::builder()
            .method("POST")
            .uri(format!("/v1/invoices/{}/pay", invoice.id))
            .header("Authorization", format!("Bearer {api_key}"))
            .header(
                "Idempotency-Key",
                format!("http-concurrency-{}-{}", invoice.id, i),
            )
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"token":"tok_success"}"#))
            .unwrap();
        calls.push(tokio::spawn(
            async move { app.oneshot(request).await.unwrap() },
        ));
    }

    let mut succeeded = 0;
    let mut rejected = 0;
    for call in calls {
        let response = call.await.unwrap();
        match response.status() {
            StatusCode::OK => succeeded += 1,
            StatusCode::UNPROCESSABLE_ENTITY | StatusCode::CONFLICT => rejected += 1,
            status => {
                let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                    .await
                    .unwrap();
                panic!(
                    "unexpected concurrent payment HTTP status {status}: {}",
                    String::from_utf8_lossy(&body)
                );
            }
        }
    }

    assert_eq!(succeeded, 1);
    assert_eq!(rejected, N - 1);
    assert_eq!(psp_counter.load(Ordering::SeqCst), 1);

    let final_invoice_status = sqlx::query_scalar::<_, String>(
        "SELECT status FROM invoices WHERE id = $1 AND business_id = $2",
    )
    .bind(invoice.id)
    .bind(business.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(final_invoice_status, "paid");

    let successful_attempts = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM payment_attempts WHERE invoice_id = $1 AND status = 'succeeded'",
    )
    .bind(invoice.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(successful_attempts, 1);
}
