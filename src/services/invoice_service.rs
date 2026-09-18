use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Transaction};
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::errors::AppError;
use crate::models::invoice::{
    CreateInvoiceRequest, Invoice, InvoiceLineItem, InvoiceResponse, InvoiceStatus,
    LineItemResponse,
};
use crate::models::payment::{
    IdempotencyRecord, PayInvoiceRequest, PayInvoiceResponse, PaymentAttempt, PaymentAttemptStatus,
};
use crate::services::psp_client::{PspChargeResult, PspClient};

pub struct InvoiceService {
    pool: PgPool,
    psp_client: PspClient,
}

enum IdempotencyClaim {
    Process { payment_attempt_id: Option<Uuid> },
    Cached(PayInvoiceResponse),
}

impl InvoiceService {
    pub fn new(pool: PgPool, psp_client: PspClient) -> Self {
        Self { pool, psp_client }
    }

    pub fn compute_request_hash(payload: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(payload.as_bytes());
        hex::encode(hasher.finalize())
    }

    // Creates an invoice and its line items in an atomic database transaction.
    // Server computes line item totals and invoice total. Client total is NEVER trusted.
    pub async fn create_invoice(
        &self,
        business_id: Uuid,
        req: CreateInvoiceRequest,
    ) -> Result<InvoiceResponse, AppError> {
        if req.line_items.is_empty() {
            return Err(AppError::BadRequest(
                "Invoice must contain at least one line item".to_string(),
            ));
        }
        if req.line_items.len() > 100 {
            return Err(AppError::BadRequest(
                "Invoice may contain at most 100 line items".to_string(),
            ));
        }
        if req.due_date <= chrono::Utc::now() {
            return Err(AppError::BadRequest(
                "Invoice due date must be in the future".to_string(),
            ));
        }

        // Validate customer exists and belongs to this business
        let customer_exists = sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM customers WHERE id = $1 AND business_id = $2",
        )
        .bind(req.customer_id)
        .bind(business_id)
        .fetch_optional(&self.pool)
        .await?;

        if customer_exists.is_none() {
            return Err(AppError::NotFound(format!(
                "Customer {} not found for this business",
                req.customer_id
            )));
        }

        let mut total_amount_cents: i64 = 0;
        for item in &req.line_items {
            if item.description.trim().is_empty() || item.description.len() > 255 {
                return Err(AppError::BadRequest(
                    "Line item description must contain 1 to 255 characters".to_string(),
                ));
            }
            if item.quantity <= 0 {
                return Err(AppError::BadRequest(
                    "Line item quantity must be positive".to_string(),
                ));
            }
            if item.unit_amount_cents < 0 {
                return Err(AppError::BadRequest(
                    "Line item unit amount must be non-negative".to_string(),
                ));
            }
            let line_total = item
                .quantity
                .checked_mul(item.unit_amount_cents)
                .ok_or_else(|| {
                    AppError::BadRequest("Integer overflow calculating line total".to_string())
                })?;

            total_amount_cents = total_amount_cents.checked_add(line_total).ok_or_else(|| {
                AppError::BadRequest("Integer overflow calculating invoice total".to_string())
            })?;
        }

        let mut tx = self.pool.begin().await?;

        // Invoices default to 'open' upon creation unless explicitly requested as draft.
        // Once open, payment attempts can be executed against it.
        let initial_status = if req.auto_open.unwrap_or(true) {
            InvoiceStatus::Open
        } else {
            InvoiceStatus::Draft
        };

        let invoice = sqlx::query_as::<_, Invoice>(
            r#"
            INSERT INTO invoices (
                business_id, customer_id, status, currency, total_amount_cents, due_date
            )
            VALUES ($1, $2, $3, 'USD', $4, $5)
            RETURNING *
            "#,
        )
        .bind(business_id)
        .bind(req.customer_id)
        .bind(initial_status.as_str())
        .bind(total_amount_cents)
        .bind(req.due_date)
        .fetch_one(&mut *tx)
        .await?;

        let mut line_item_responses = Vec::new();

        for item in req.line_items {
            let line_total = item.quantity * item.unit_amount_cents;
            let line_record = sqlx::query_as::<_, InvoiceLineItem>(
                r#"
                INSERT INTO invoice_line_items (
                    invoice_id, description, quantity, unit_amount_cents, total_amount_cents
                )
                VALUES ($1, $2, $3, $4, $5)
                RETURNING *
                "#,
            )
            .bind(invoice.id)
            .bind(&item.description)
            .bind(item.quantity)
            .bind(item.unit_amount_cents)
            .bind(line_total)
            .fetch_one(&mut *tx)
            .await?;

            line_item_responses.push(LineItemResponse {
                id: line_record.id,
                description: line_record.description,
                quantity: line_record.quantity,
                unit_amount_cents: line_record.unit_amount_cents,
                total_amount_cents: line_record.total_amount_cents,
            });
        }

        // Webhook event enqueue
        let payload = serde_json::json!({
            "id": invoice.id,
            "business_id": invoice.business_id,
            "customer_id": invoice.customer_id,
            "status": invoice.status,
            "total_amount_cents": invoice.total_amount_cents,
            "due_date": invoice.due_date
        });
        self.enqueue_webhook_event(&mut tx, business_id, "invoice.created", payload)
            .await?;

        tx.commit().await?;

        Ok(InvoiceResponse {
            id: invoice.id,
            business_id: invoice.business_id,
            customer_id: invoice.customer_id,
            status: invoice.status,
            currency: invoice.currency,
            total_amount_cents: invoice.total_amount_cents,
            due_date: invoice.due_date,
            line_items: line_item_responses,
            created_at: invoice.created_at,
            updated_at: invoice.updated_at,
        })
    }

    pub async fn get_invoice(
        &self,
        business_id: Uuid,
        invoice_id: Uuid,
    ) -> Result<InvoiceResponse, AppError> {
        let invoice = sqlx::query_as::<_, Invoice>(
            "SELECT * FROM invoices WHERE id = $1 AND business_id = $2",
        )
        .bind(invoice_id)
        .bind(business_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Invoice {} not found", invoice_id)))?;

        let line_items = sqlx::query_as::<_, InvoiceLineItem>(
            "SELECT * FROM invoice_line_items WHERE invoice_id = $1 ORDER BY created_at ASC",
        )
        .bind(invoice.id)
        .fetch_all(&self.pool)
        .await?;

        let line_responses = line_items
            .into_iter()
            .map(|li| LineItemResponse {
                id: li.id,
                description: li.description,
                quantity: li.quantity,
                unit_amount_cents: li.unit_amount_cents,
                total_amount_cents: li.total_amount_cents,
            })
            .collect();

        Ok(InvoiceResponse {
            id: invoice.id,
            business_id: invoice.business_id,
            customer_id: invoice.customer_id,
            status: invoice.status,
            currency: invoice.currency,
            total_amount_cents: invoice.total_amount_cents,
            due_date: invoice.due_date,
            line_items: line_responses,
            created_at: invoice.created_at,
            updated_at: invoice.updated_at,
        })
    }

    pub async fn transition_invoice(
        &self,
        business_id: Uuid,
        invoice_id: Uuid,
        target: InvoiceStatus,
    ) -> Result<InvoiceResponse, AppError> {
        let mut tx = self.pool.begin().await?;
        let invoice = sqlx::query_as::<_, Invoice>(
            "SELECT * FROM invoices WHERE id = $1 AND business_id = $2 FOR UPDATE",
        )
        .bind(invoice_id)
        .bind(business_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Invoice {invoice_id} not found")))?;

        let current = InvoiceStatus::from_str_strict(&invoice.status)?;
        if !current.can_transition_to(target) || current == target {
            return Err(AppError::InvalidStateTransition(format!(
                "Cannot transition invoice from '{}' to '{}'",
                current.as_str(),
                target.as_str()
            )));
        }

        sqlx::query("UPDATE invoices SET status = $1, updated_at = NOW() WHERE id = $2")
            .bind(target.as_str())
            .bind(invoice_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        self.get_invoice(business_id, invoice_id).await
    }

    pub async fn list_invoices(
        &self,
        business_id: Uuid,
        status_filter: Option<String>,
    ) -> Result<Vec<InvoiceResponse>, AppError> {
        let invoices = if let Some(ref status) = status_filter {
            sqlx::query_as::<_, Invoice>(
                "SELECT * FROM invoices WHERE business_id = $1 AND status = $2 ORDER BY created_at DESC",
            )
            .bind(business_id)
            .bind(status)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query_as::<_, Invoice>(
                "SELECT * FROM invoices WHERE business_id = $1 ORDER BY created_at DESC",
            )
            .bind(business_id)
            .fetch_all(&self.pool)
            .await?
        };

        let mut results = Vec::with_capacity(invoices.len());
        for inv in invoices {
            let line_items = sqlx::query_as::<_, InvoiceLineItem>(
                "SELECT * FROM invoice_line_items WHERE invoice_id = $1 ORDER BY created_at ASC",
            )
            .bind(inv.id)
            .fetch_all(&self.pool)
            .await?;

            let line_responses = line_items
                .into_iter()
                .map(|li| LineItemResponse {
                    id: li.id,
                    description: li.description,
                    quantity: li.quantity,
                    unit_amount_cents: li.unit_amount_cents,
                    total_amount_cents: li.total_amount_cents,
                })
                .collect();

            results.push(InvoiceResponse {
                id: inv.id,
                business_id: inv.business_id,
                customer_id: inv.customer_id,
                status: inv.status,
                currency: inv.currency,
                total_amount_cents: inv.total_amount_cents,
                due_date: inv.due_date,
                line_items: line_responses,
                created_at: inv.created_at,
                updated_at: inv.updated_at,
            });
        }

        Ok(results)
    }

    // Executes a payment attempt with idempotency, concurrency locking, and safe PSP handling.
    pub async fn pay_invoice(
        &self,
        business_id: Uuid,
        invoice_id: Uuid,
        req: PayInvoiceRequest,
        idempotency_key: Option<String>,
        raw_body: &str,
    ) -> Result<PayInvoiceResponse, AppError> {
        let request_hash = Self::compute_request_hash(raw_body);
        let idempotency_key = idempotency_key
            .filter(|key| !key.trim().is_empty() && key.len() <= 255)
            .ok_or(AppError::IdempotencyKeyRequired)?;
        let request_path = format!("/v1/invoices/{}/pay", invoice_id);

        let existing_payment_attempt_id = match self
            .claim_idempotency(business_id, &request_path, &idempotency_key, &request_hash)
            .await?
        {
            IdempotencyClaim::Cached(response) => return Ok(response),
            IdempotencyClaim::Process { payment_attempt_id } => payment_attempt_id,
        };

        // Start transaction for pessimistic locking
        let mut tx = self.pool.begin().await?;

        // A row-level lock (`SELECT ... FOR UPDATE`) serializes payment attempts for this invoice
        // across service replicas. An in-memory lock would not work across replicas, and a
        // compare-and-swap alone cannot coordinate a PSP call that was already dispatched.
        // A separate key can be rejected with 409 before the PSP call while another
        // payment operation is unresolved. Once the first request settles, later
        // requests observe status == 'paid' and receive 422 without a PSP call.
        let invoice = sqlx::query_as::<_, Invoice>(
            r#"
            SELECT * FROM invoices 
            WHERE id = $1 AND business_id = $2 
            FOR UPDATE
            "#,
        )
        .bind(invoice_id)
        .bind(business_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Invoice {} not found", invoice_id)))?;

        // Validate state transitions:
        // - Only 'open' invoices may accept payments.
        // - If invoice is already 'paid', reject immediately with 422 Unprocessable Entity.
        // - If invoice is 'void' or 'uncollectible', reject.
        // - If invoice is 'draft', it must be finalized/opened first.
        let current_status = InvoiceStatus::from_str_strict(&invoice.status)?;
        if current_status != InvoiceStatus::Open {
            sqlx::query(
                "DELETE FROM idempotency_records WHERE business_id = $1 AND request_path = $2 AND idempotency_key = $3 AND status = 'in_progress'",
            )
            .bind(business_id)
            .bind(&request_path)
            .bind(&idempotency_key)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            return Err(AppError::InvalidStateTransition(format!(
                "Cannot pay invoice: current state is '{}'. Payments can only be processed on 'open' invoices.",
                invoice.status
            )));
        }

        // An invoice with another unresolved processor operation must not accept a
        // fresh key: the original PSP request may still settle. Only that original
        // key may reconcile the outcome.
        let other_unresolved_operation = sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM idempotency_records WHERE business_id = $1 AND request_path = $2 AND idempotency_key <> $3 AND status IN ('in_progress', 'unknown') LIMIT 1",
        )
        .bind(business_id)
        .bind(&request_path)
        .bind(&idempotency_key)
        .fetch_optional(&mut *tx)
        .await?;
        if other_unresolved_operation.is_some() {
            sqlx::query(
                "DELETE FROM idempotency_records WHERE business_id = $1 AND request_path = $2 AND idempotency_key = $3 AND status = 'in_progress'",
            )
            .bind(business_id)
            .bind(&request_path)
            .bind(&idempotency_key)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            return Err(AppError::IdempotencyConflict(
                "A previous payment has an unresolved processor outcome. Retry it with its original Idempotency-Key.".to_string(),
            ));
        }

        // Create pending payment attempt record
        let payment_attempt = if let Some(payment_attempt_id) = existing_payment_attempt_id {
            let attempt = sqlx::query_as::<_, PaymentAttempt>(
                "SELECT * FROM payment_attempts WHERE id = $1 AND invoice_id = $2 FOR UPDATE",
            )
            .bind(payment_attempt_id)
            .bind(invoice.id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| {
                AppError::Internal(
                    "Idempotency record references a missing payment attempt".to_string(),
                )
            })?;

            sqlx::query(
                "UPDATE payment_attempts SET status = 'pending', error_code = NULL, updated_at = NOW() WHERE id = $1",
            )
            .bind(attempt.id)
            .execute(&mut *tx)
            .await?;
            attempt
        } else {
            let attempt = sqlx::query_as::<_, PaymentAttempt>(
                r#"
                INSERT INTO payment_attempts (
                    invoice_id, idempotency_key, amount_cents, status, payment_method_reference
                )
                VALUES ($1, $2, $3, $4, $5)
                RETURNING *
                "#,
            )
            .bind(invoice.id)
            .bind(&idempotency_key)
            .bind(invoice.total_amount_cents)
            .bind(PaymentAttemptStatus::Pending.as_str())
            // Never persist the submitted bearer token. A live integration would keep the
            // PSP's payment-method reference here after tokenization.
            .bind("mock_payment_method")
            .fetch_one(&mut *tx)
            .await?;

            sqlx::query(
                "UPDATE idempotency_records SET payment_attempt_id = $1, updated_at = NOW() WHERE business_id = $2 AND request_path = $3 AND idempotency_key = $4",
            )
            .bind(attempt.id)
            .bind(business_id)
            .bind(&request_path)
            .bind(&idempotency_key)
            .execute(&mut *tx)
            .await?;
            attempt
        };

        // A timeout, a transport failure, or a PSP idempotency-in-progress response
        // leaves the processor outcome unknown. The invoice remains open, but a new
        // key is blocked until the original key resolves it. The mock's explicit 500
        // is instead a failed, retryable attempt.
        let charge_result = self
            .psp_client
            .charge(
                invoice.total_amount_cents,
                &invoice.currency,
                &req.token,
                Some(&idempotency_key),
            )
            .await;
        let outcome_is_ambiguous = matches!(
            &charge_result,
            PspChargeResult::Timeout
                | PspChargeResult::InProgress
                | PspChargeResult::NetworkError { .. }
        );

        let response: Result<PayInvoiceResponse, AppError> = match charge_result {
            PspChargeResult::Success { psp_ref } => {
                info!(
                    invoice_id = %invoice.id,
                    psp_ref = %psp_ref,
                    "Payment confirmed by PSP. Transitioning invoice to 'paid'."
                );

                // Update payment attempt
                sqlx::query(
                    r#"
                    UPDATE payment_attempts
                    SET status = $1, psp_reference = $2, updated_at = NOW()
                    WHERE id = $3
                    "#,
                )
                .bind(PaymentAttemptStatus::Succeeded.as_str())
                .bind(&psp_ref)
                .bind(payment_attempt.id)
                .execute(&mut *tx)
                .await?;

                // Transition invoice to terminal state 'paid'
                sqlx::query(
                    r#"
                    UPDATE invoices
                    SET status = $1, updated_at = NOW()
                    WHERE id = $2
                    "#,
                )
                .bind(InvoiceStatus::Paid.as_str())
                .bind(invoice.id)
                .execute(&mut *tx)
                .await?;

                // Enqueue invoice.paid webhook
                let event_payload = serde_json::json!({
                    "invoice_id": invoice.id,
                    "business_id": invoice.business_id,
                    "payment_attempt_id": payment_attempt.id,
                    "amount_cents": invoice.total_amount_cents,
                    "psp_reference": psp_ref,
                    "status": "paid"
                });
                self.enqueue_webhook_event(&mut tx, business_id, "invoice.paid", event_payload)
                    .await?;

                let resp = PayInvoiceResponse {
                    invoice_id: invoice.id,
                    payment_attempt_id: payment_attempt.id,
                    status: "succeeded".to_string(),
                    invoice_status: "paid".to_string(),
                    psp_reference: Some(psp_ref),
                    error_code: None,
                    message: "Payment successfully processed".to_string(),
                };
                Ok(resp)
            }
            PspChargeResult::Declined { code } => {
                warn!(
                    invoice_id = %invoice.id,
                    error_code = %code,
                    "Payment declined by PSP. Invoice remains in 'open' state."
                );

                // Update payment attempt to failed
                sqlx::query(
                    r#"
                    UPDATE payment_attempts
                    SET status = $1, error_code = $2, updated_at = NOW()
                    WHERE id = $3
                    "#,
                )
                .bind(PaymentAttemptStatus::Failed.as_str())
                .bind(&code)
                .bind(payment_attempt.id)
                .execute(&mut *tx)
                .await?;

                // Enqueue invoice.payment_failed webhook
                let event_payload = serde_json::json!({
                    "invoice_id": invoice.id,
                    "business_id": invoice.business_id,
                    "payment_attempt_id": payment_attempt.id,
                    "amount_cents": invoice.total_amount_cents,
                    "error_code": code,
                    "status": "failed"
                });
                self.enqueue_webhook_event(
                    &mut tx,
                    business_id,
                    "invoice.payment_failed",
                    event_payload,
                )
                .await?;

                let resp = PayInvoiceResponse {
                    invoice_id: invoice.id,
                    payment_attempt_id: payment_attempt.id,
                    status: "failed".to_string(),
                    invoice_status: "open".to_string(),
                    psp_reference: None,
                    error_code: Some(code.clone()),
                    message: format!("Payment declined: {}", code),
                };
                Ok(resp)
            }
            PspChargeResult::Timeout => {
                warn!(
                    invoice_id = %invoice.id,
                    "PSP timed out. Invoice remains in 'open' state."
                );

                sqlx::query(
                    r#"
                    UPDATE payment_attempts
                    SET status = $1, error_code = 'psp_outcome_unknown', updated_at = NOW()
                    WHERE id = $2
                    "#,
                )
                .bind(PaymentAttemptStatus::Unknown.as_str())
                .bind(payment_attempt.id)
                .execute(&mut *tx)
                .await?;

                Err(AppError::PspTimeout(
                    "Payment processor timed out; its outcome is unknown. Retry only with the same Idempotency-Key.".to_string(),
                ))
            }
            PspChargeResult::InProgress => {
                sqlx::query(
                    "UPDATE payment_attempts SET status = 'unknown', error_code = 'psp_outcome_unknown', updated_at = NOW() WHERE id = $1",
                )
                .bind(payment_attempt.id)
                .execute(&mut *tx)
                .await?;
                Err(AppError::PspTimeout(
                    "Payment processor is still resolving this Idempotency-Key. Retry shortly with the same key.".to_string(),
                ))
            }
            PspChargeResult::RetryableFailure { message } => {
                sqlx::query(
                    "UPDATE payment_attempts SET status = 'failed', error_code = 'psp_network_error', updated_at = NOW() WHERE id = $1",
                )
                .bind(payment_attempt.id)
                .execute(&mut *tx)
                .await?;
                let event_payload = serde_json::json!({
                    "invoice_id": invoice.id,
                    "business_id": invoice.business_id,
                    "payment_attempt_id": payment_attempt.id,
                    "amount_cents": invoice.total_amount_cents,
                    "error_code": "psp_network_error",
                    "status": "failed"
                });
                self.enqueue_webhook_event(
                    &mut tx,
                    business_id,
                    "invoice.payment_failed",
                    event_payload,
                )
                .await?;
                Err(AppError::PspNetworkError(format!(
                    "Payment processor returned a retryable server failure: {message}"
                )))
            }
            PspChargeResult::NetworkError { message } => {
                error!(
                    invoice_id = %invoice.id,
                    error = %message,
                    "PSP network error. Invoice remains in 'open' state."
                );

                sqlx::query(
                    r#"
                    UPDATE payment_attempts
                    SET status = $1, error_code = 'psp_outcome_unknown', updated_at = NOW()
                    WHERE id = $2
                    "#,
                )
                .bind(PaymentAttemptStatus::Unknown.as_str())
                .bind(payment_attempt.id)
                .execute(&mut *tx)
                .await?;

                Err(AppError::PspNetworkError(format!(
                    "Payment processor network failure: {}. Its outcome is unknown; retry only with the same Idempotency-Key.",
                    message
                )))
            }
        };

        match &response {
            Ok(resp) => {
                let response_status = if resp.status == "failed" { 402 } else { 200 };
                let response_body = serde_json::to_string(resp).map_err(|error| {
                    AppError::Internal(format!("Could not serialize idempotent response: {error}"))
                })?;
                sqlx::query(
                    "UPDATE idempotency_records SET status = 'completed', response_status_code = $1, response_body = $2, updated_at = NOW() WHERE business_id = $3 AND request_path = $4 AND idempotency_key = $5",
                )
                .bind(response_status)
                .bind(response_body)
                .bind(business_id)
                .bind(&request_path)
                .bind(&idempotency_key)
                .execute(&mut *tx)
                .await?;
            }
            Err(_) if outcome_is_ambiguous => {
                sqlx::query(
                    "UPDATE idempotency_records SET status = 'unknown', updated_at = NOW() WHERE business_id = $1 AND request_path = $2 AND idempotency_key = $3",
                )
                .bind(business_id)
                .bind(&request_path)
                .bind(&idempotency_key)
                .execute(&mut *tx)
                .await?;
            }
            Err(_) => {
                sqlx::query(
                    "DELETE FROM idempotency_records WHERE business_id = $1 AND request_path = $2 AND idempotency_key = $3",
                )
                .bind(business_id)
                .bind(&request_path)
                .bind(&idempotency_key)
                .execute(&mut *tx)
                .await?;
            }
        }

        tx.commit().await?;

        response
    }

    async fn claim_idempotency(
        &self,
        business_id: Uuid,
        request_path: &str,
        key: &str,
        request_hash: &str,
    ) -> Result<IdempotencyClaim, AppError> {
        let inserted = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO idempotency_records (business_id, idempotency_key, request_path, request_hash, status) VALUES ($1, $2, $3, $4, 'in_progress') ON CONFLICT (business_id, request_path, idempotency_key) DO NOTHING RETURNING id",
        )
        .bind(business_id)
        .bind(key)
        .bind(request_path)
        .bind(request_hash)
        .fetch_optional(&self.pool)
        .await?;

        if inserted.is_some() {
            return Ok(IdempotencyClaim::Process {
                payment_attempt_id: None,
            });
        }

        let record = sqlx::query_as::<_, IdempotencyRecord>(
            "SELECT * FROM idempotency_records WHERE business_id = $1 AND request_path = $2 AND idempotency_key = $3",
        )
        .bind(business_id)
        .bind(request_path)
        .bind(key)
        .fetch_one(&self.pool)
        .await?;

        if record.request_hash != request_hash {
            return Err(AppError::IdempotencyConflict(
                "Idempotency key was previously used with a different request payload".to_string(),
            ));
        }
        if record.status == "completed" {
            let body = record.response_body.ok_or_else(|| {
                AppError::Internal("Completed idempotency record has no response body".to_string())
            })?;
            let response = serde_json::from_str(&body).map_err(|_| {
                AppError::Internal(
                    "Completed idempotency record has an invalid response body".to_string(),
                )
            })?;
            return Ok(IdempotencyClaim::Cached(response));
        }
        if record.status == "unknown" {
            let claimed = sqlx::query_scalar::<_, Uuid>(
                "UPDATE idempotency_records SET status = 'in_progress', updated_at = NOW() WHERE id = $1 AND status = 'unknown' RETURNING id",
            )
            .bind(record.id)
            .fetch_optional(&self.pool)
            .await?;
            if claimed.is_some() {
                return Ok(IdempotencyClaim::Process {
                    payment_attempt_id: record.payment_attempt_id,
                });
            }
        }

        // A process crash after the PSP accepts a charge can leave an in-progress
        // claim behind. After the bounded lease, reclaim it and rely on the same
        // downstream idempotency key, assuming the provider honors durable idempotency.
        if record.status == "in_progress" {
            let reclaimed = sqlx::query_scalar::<_, Uuid>(
                "UPDATE idempotency_records SET updated_at = NOW() WHERE id = $1 AND status = 'in_progress' AND updated_at < NOW() - INTERVAL '30 seconds' RETURNING id",
            )
            .bind(record.id)
            .fetch_optional(&self.pool)
            .await?;
            if reclaimed.is_some() {
                return Ok(IdempotencyClaim::Process {
                    payment_attempt_id: record.payment_attempt_id,
                });
            }
        }

        Err(AppError::IdempotencyConflict(
            "A payment request with this idempotency key is already in progress. Please retry shortly.".to_string(),
        ))
    }

    // Helper to insert a webhook event and queue deliveries for all active business endpoints
    pub async fn enqueue_webhook_event(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        business_id: Uuid,
        event_type: &str,
        payload: serde_json::Value,
    ) -> Result<(), sqlx::Error> {
        let event_id = Uuid::new_v4();
        let mut payload = payload;
        if let Some(object) = payload.as_object_mut() {
            object.insert("event_id".to_string(), serde_json::json!(event_id));
            object.insert("event_type".to_string(), serde_json::json!(event_type));
        }
        sqlx::query(
            r#"
            INSERT INTO webhook_events (id, business_id, event_type, payload)
            VALUES ($1, $2, $3, $4)
            "#,
        )
        .bind(event_id)
        .bind(business_id)
        .bind(event_type)
        .bind(payload)
        .execute(&mut **tx)
        .await?;

        let endpoint_ids = sqlx::query_scalar::<_, Uuid>(
            r#"
            SELECT id FROM webhook_endpoints
            WHERE business_id = $1 AND is_active = TRUE
            "#,
        )
        .bind(business_id)
        .fetch_all(&mut **tx)
        .await?;

        for ep_id in endpoint_ids {
            sqlx::query(
                r#"
                INSERT INTO webhook_deliveries (
                    webhook_event_id, webhook_endpoint_id, status, next_retry_at
                )
                VALUES ($1, $2, 'pending', NOW())
                "#,
            )
            .bind(event_id)
            .bind(ep_id)
            .execute(&mut **tx)
            .await?;
        }

        Ok(())
    }
}
