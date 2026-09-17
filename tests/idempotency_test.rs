mod common;

use chrono::Utc;
use std::sync::atomic::Ordering;

use dodo_payments::models::invoice::{CreateInvoiceRequest, CreateLineItemRequest};
use dodo_payments::models::payment::{PayInvoiceRequest, PaymentAttempt};

// REQUIRED TEST 2: Idempotency Test
// Spec: "One idempotency test that retries the same request with the same key and asserts the same
// response is returned without a second PSP call."
#[tokio::test]
async fn test_payment_idempotency_same_key_no_duplicate_psp_call() {
    let ctx = common::setup_test_context().await;
    let (pool, service, psp_counter, business, customer) = ctx;

    // 1. Create an open invoice
    let inv_req = CreateInvoiceRequest {
        customer_id: customer.id,
        due_date: Utc::now() + chrono::Duration::days(7),
        line_items: vec![CreateLineItemRequest {
            description: "Yearly SaaS License".to_string(),
            quantity: 1,
            unit_amount_cents: 12000,
        }],
        auto_open: Some(true),
    };
    let invoice = service
        .create_invoice(business.id, inv_req)
        .await
        .expect("Failed to create invoice");

    let idempotency_key = format!("idemp_key_{}", invoice.id);
    let pay_req = PayInvoiceRequest {
        token: "tok_success".to_string(),
    };
    let raw_payload = serde_json::to_string(&pay_req).unwrap();

    // 2. First call: Should contact PSP and succeed
    let first_resp = service
        .pay_invoice(
            business.id,
            invoice.id,
            pay_req,
            Some(idempotency_key.clone()),
            &raw_payload,
        )
        .await
        .expect("First payment call should succeed");

    assert_eq!(first_resp.status, "succeeded");
    assert_eq!(first_resp.invoice_status, "paid");
    assert!(first_resp.psp_reference.is_some());
    assert_eq!(psp_counter.load(Ordering::SeqCst), 1);

    // 3. Second call: Retry with identical idempotency key and identical payload
    let pay_req_retry = PayInvoiceRequest {
        token: "tok_success".to_string(),
    };
    let second_resp = service
        .pay_invoice(
            business.id,
            invoice.id,
            pay_req_retry,
            Some(idempotency_key),
            &raw_payload,
        )
        .await
        .expect("Retried payment call should succeed idempotently");

    // Assert: Responses match exactly
    assert_eq!(second_resp.status, first_resp.status);
    assert_eq!(second_resp.invoice_status, first_resp.invoice_status);
    assert_eq!(
        second_resp.payment_attempt_id,
        first_resp.payment_attempt_id
    );
    assert_eq!(second_resp.psp_reference, first_resp.psp_reference);

    // Assert: PSP was NOT called a second time
    assert_eq!(
        psp_counter.load(Ordering::SeqCst),
        1,
        "PSP call counter must remain 1 after idempotent retry"
    );

    // Assert: Only 1 payment attempt stored in database
    let attempts =
        sqlx::query_as::<_, PaymentAttempt>("SELECT * FROM payment_attempts WHERE invoice_id = $1")
            .bind(invoice.id)
            .fetch_all(&pool)
            .await
            .unwrap();

    assert_eq!(attempts.len(), 1);
}

#[tokio::test]
async fn test_idempotency_key_is_scoped_to_the_invoice_operation() {
    let (_pool, service, psp_counter, business, customer) = common::setup_test_context().await;

    let create_invoice = || CreateInvoiceRequest {
        customer_id: customer.id,
        due_date: Utc::now() + chrono::Duration::days(7),
        line_items: vec![CreateLineItemRequest {
            description: "Scoped idempotency".to_string(),
            quantity: 1,
            unit_amount_cents: 1000,
        }],
        auto_open: Some(true),
    };
    let first_invoice = service
        .create_invoice(business.id, create_invoice())
        .await
        .unwrap();
    let second_invoice = service
        .create_invoice(business.id, create_invoice())
        .await
        .unwrap();
    let request = PayInvoiceRequest {
        token: "tok_success".to_string(),
    };
    let body = serde_json::to_string(&request).unwrap();
    let key = "same-key-on-different-invoices".to_string();

    let first = service
        .pay_invoice(
            business.id,
            first_invoice.id,
            request.clone(),
            Some(key.clone()),
            &body,
        )
        .await
        .unwrap();
    let second = service
        .pay_invoice(business.id, second_invoice.id, request, Some(key), &body)
        .await
        .unwrap();

    assert_eq!(first.invoice_id, first_invoice.id);
    assert_eq!(second.invoice_id, second_invoice.id);
    assert_eq!(psp_counter.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn test_concurrent_same_key_calls_only_charge_once() {
    let (_pool, service, psp_counter, business, customer) = common::setup_test_context().await;
    let invoice = service
        .create_invoice(
            business.id,
            CreateInvoiceRequest {
                customer_id: customer.id,
                due_date: Utc::now() + chrono::Duration::days(7),
                line_items: vec![CreateLineItemRequest {
                    description: "Same-key race".to_string(),
                    quantity: 1,
                    unit_amount_cents: 1000,
                }],
                auto_open: Some(true),
            },
        )
        .await
        .unwrap();
    let request = PayInvoiceRequest {
        token: "tok_success".to_string(),
    };
    let body = serde_json::to_string(&request).unwrap();
    let mut calls = Vec::new();
    for _ in 0..10 {
        let service = service.clone();
        let request = request.clone();
        let body = body.clone();
        calls.push(tokio::spawn(async move {
            service
                .pay_invoice(
                    business.id,
                    invoice.id,
                    request,
                    Some("single-concurrent-key".to_string()),
                    &body,
                )
                .await
        }));
    }
    for call in calls {
        let _ = call.await.unwrap();
    }

    assert_eq!(psp_counter.load(Ordering::SeqCst), 1);
    let replay = service
        .pay_invoice(
            business.id,
            invoice.id,
            request,
            Some("single-concurrent-key".to_string()),
            &body,
        )
        .await
        .unwrap();
    assert_eq!(replay.status, "succeeded");
}
