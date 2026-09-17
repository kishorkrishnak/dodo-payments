mod common;

use chrono::Utc;
use std::sync::atomic::Ordering;

use dodo_payments::models::invoice::{CreateInvoiceRequest, CreateLineItemRequest, Invoice};
use dodo_payments::models::payment::{PayInvoiceRequest, PaymentAttempt};

// REQUIRED TEST 1: Concurrency Test
// Spec: "One concurrency test that fires N concurrent POST /pay requests for the same invoice
// and asserts that at most one succeeds, no double-charges occur, and the final state is consistent."
#[tokio::test]
async fn test_concurrent_payments_no_double_charge() {
    let ctx = common::setup_test_context().await;
    let (pool, service, psp_counter, business, customer) = ctx;

    // 1. Create an open invoice for $50.00 (5000 cents)
    let inv_req = CreateInvoiceRequest {
        customer_id: customer.id,
        due_date: Utc::now() + chrono::Duration::days(7),
        line_items: vec![CreateLineItemRequest {
            description: "Pro Plan Subscription".to_string(),
            quantity: 1,
            unit_amount_cents: 5000,
        }],
        auto_open: Some(true),
    };
    let invoice = service
        .create_invoice(business.id, inv_req)
        .await
        .expect("Failed to create test invoice");

    assert_eq!(invoice.status, "open");
    assert_eq!(invoice.total_amount_cents, 5000);

    // 2. Fire N concurrent payment attempts at the exact same instant
    const N: usize = 10;
    let mut handles = Vec::with_capacity(N);

    for i in 0..N {
        let service_clone = service.clone();
        let biz_id = business.id;
        let inv_id = invoice.id;
        let idemp_key = format!("concurrent_idemp_{}_{}", inv_id, i);

        let handle = tokio::spawn(async move {
            let req = PayInvoiceRequest {
                token: "tok_success".to_string(),
            };
            let raw_body = serde_json::to_string(&req).unwrap();
            service_clone
                .pay_invoice(biz_id, inv_id, req, Some(idemp_key), &raw_body)
                .await
        });
        handles.push(handle);
    }

    let mut succeeded_count = 0;
    let mut rejected_count = 0;

    for handle in handles {
        let res = handle.await.unwrap();
        match res {
            Ok(resp) => {
                assert_eq!(resp.status, "succeeded");
                assert_eq!(resp.invoice_status, "paid");
                succeeded_count += 1;
            }
            Err(err) => {
                // Should be rejected due to invalid state transition (already paid)
                let code = err.error_code();
                assert_eq!(code, "invalid_state_transition");
                rejected_count += 1;
            }
        }
    }

    // Assert: Exactly ONE succeeds and N-1 are rejected
    assert_eq!(
        succeeded_count, 1,
        "Exactly one concurrent payment attempt must succeed"
    );
    assert_eq!(
        rejected_count,
        N - 1,
        "All other concurrent payment attempts must be rejected"
    );

    // Assert: Verify database consistency
    let final_invoice = sqlx::query_as::<_, Invoice>("SELECT * FROM invoices WHERE id = $1")
        .bind(invoice.id)
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(final_invoice.status, "paid");

    let successful_attempts = sqlx::query_as::<_, PaymentAttempt>(
        "SELECT * FROM payment_attempts WHERE invoice_id = $1 AND status = 'succeeded'",
    )
    .bind(invoice.id)
    .fetch_all(&pool)
    .await
    .unwrap();

    assert_eq!(
        successful_attempts.len(),
        1,
        "There must be exactly one successful payment attempt recorded"
    );

    // Assert: External PSP was contacted exactly once (no double-charging!)
    assert_eq!(
        psp_counter.load(Ordering::SeqCst),
        1,
        "PSP must only be called once; no double-charges may occur"
    );
}
