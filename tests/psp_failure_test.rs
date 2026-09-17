mod common;

use chrono::Utc;

use dodo_payments::errors::AppError;
use dodo_payments::models::invoice::{CreateInvoiceRequest, CreateLineItemRequest, Invoice};
use dodo_payments::models::payment::{PayInvoiceRequest, PaymentAttempt};

// REQUIRED TEST 3: PSP Failure & Timeout Handling Test
// Spec: "One PSP-failure test that uses tok_timeout or tok_network_error and asserts the invoice
// is not stuck in a bad state."
#[tokio::test]
async fn test_psp_timeout_and_network_error_do_not_corrupt_invoice_state() {
    let ctx = common::setup_test_context().await;
    let (pool, service, _psp_counter, business, customer) = ctx;

    // 1. Create an invoice in 'open' state
    let inv_req = CreateInvoiceRequest {
        customer_id: customer.id,
        due_date: Utc::now() + chrono::Duration::days(7),
        line_items: vec![CreateLineItemRequest {
            description: "Consulting Hour".to_string(),
            quantity: 1,
            unit_amount_cents: 15000,
        }],
        auto_open: Some(true),
    };
    let invoice = service
        .create_invoice(business.id, inv_req)
        .await
        .expect("Failed to create invoice");

    assert_eq!(invoice.status, "open");

    // 2. Attempt payment with `tok_timeout`
    // In our test context, client timeout is set to 1 second while mock sleeps 3 seconds
    let pay_timeout_req = PayInvoiceRequest {
        token: "tok_timeout".to_string(),
    };
    let raw_payload_timeout = serde_json::to_string(&pay_timeout_req).unwrap();

    let timeout_result = service
        .pay_invoice(
            business.id,
            invoice.id,
            pay_timeout_req,
            Some(format!("timeout_key_{}", invoice.id)),
            &raw_payload_timeout,
        )
        .await;

    // Assert: Endpoint returned PspTimeout (HTTP 504 equivalent) without hanging
    match timeout_result {
        Err(AppError::PspTimeout(_)) => (),
        other => panic!("Expected PspTimeout, but got: {:?}", other),
    }

    // Assert: Invoice is STILL in 'open' state (not stuck in paying or failed)
    let inv_after_timeout = sqlx::query_as::<_, Invoice>("SELECT * FROM invoices WHERE id = $1")
        .bind(invoice.id)
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(
        inv_after_timeout.status, "open",
        "Invoice status must remain 'open' following a PSP timeout"
    );

    // Assert: An unknown-outcome attempt is recorded. A timeout is not proof that
    // the PSP did not charge, so callers must reuse the same idempotency key.
    let attempts = sqlx::query_as::<_, PaymentAttempt>(
        "SELECT * FROM payment_attempts WHERE invoice_id = $1 ORDER BY created_at DESC",
    )
    .bind(invoice.id)
    .fetch_all(&pool)
    .await
    .unwrap();

    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].status, "unknown");
    assert_eq!(
        attempts[0].error_code.as_deref(),
        Some("psp_outcome_unknown")
    );

    // 3. A different key cannot bypass an unresolved timeout outcome.
    let conflicting_result = service
        .pay_invoice(
            business.id,
            invoice.id,
            PayInvoiceRequest {
                token: "tok_success".to_string(),
            },
            Some(format!("different_key_{}", invoice.id)),
            r#"{"token":"tok_success"}"#,
        )
        .await;
    assert!(matches!(
        conflicting_result,
        Err(AppError::IdempotencyConflict(_))
    ));

    // 4. HTTP 500 is an explicit mock-PSP failure, not an ambiguous outcome.
    let network_invoice = service
        .create_invoice(
            business.id,
            CreateInvoiceRequest {
                customer_id: customer.id,
                due_date: Utc::now() + chrono::Duration::days(7),
                line_items: vec![CreateLineItemRequest {
                    description: "Network failure invoice".to_string(),
                    quantity: 1,
                    unit_amount_cents: 15000,
                }],
                auto_open: Some(true),
            },
        )
        .await
        .expect("Failed to create network-failure invoice");

    let pay_net_req = PayInvoiceRequest {
        token: "tok_network_error".to_string(),
    };
    let raw_payload_net = serde_json::to_string(&pay_net_req).unwrap();

    let net_result = service
        .pay_invoice(
            business.id,
            network_invoice.id,
            pay_net_req,
            Some(format!("net_key_{}", invoice.id)),
            &raw_payload_net,
        )
        .await;

    // Assert: Handled as PspNetworkError (HTTP 502 equivalent)
    match net_result {
        Err(AppError::PspNetworkError(_)) => (),
        other => panic!("Expected PspNetworkError, but got: {:?}", other),
    }

    // Assert: Invoice is STILL 'open' and can use a new key after explicit 500.
    let inv_after_net = sqlx::query_as::<_, Invoice>("SELECT * FROM invoices WHERE id = $1")
        .bind(network_invoice.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(inv_after_net.status, "open");

    // 5. Retry with `tok_success`: Verify customer can now pay successfully
    let pay_success_req = PayInvoiceRequest {
        token: "tok_success".to_string(),
    };
    let raw_payload_success = serde_json::to_string(&pay_success_req).unwrap();

    let success_result = service
        .pay_invoice(
            business.id,
            network_invoice.id,
            pay_success_req,
            Some(format!("success_retry_key_{}", invoice.id)),
            &raw_payload_success,
        )
        .await
        .expect("Subsequent payment attempt after PSP recovery must succeed");

    assert_eq!(success_result.status, "succeeded");
    assert_eq!(success_result.invoice_status, "paid");

    let final_invoice = sqlx::query_as::<_, Invoice>("SELECT * FROM invoices WHERE id = $1")
        .bind(network_invoice.id)
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(final_invoice.status, "paid");
}
