# Dodo Payments - Invoice & Payment Core Service

A minimal, resilient, and production-grade Invoice & Payment Service built with **Rust (Axum)**, **SQLx**, and **PostgreSQL**.

---

## Demo Video

- **Video Link**: `[DEMO VIDEO LINK - Loom / Google Drive / S3]` *(Insert your unscripted 5–10 minute screen recording link here)*
- **Video Agenda**:
  1. **Architecture Overview (1–2 min)**: Services (`invoice-service`, `mock-psp`, `postgres`), data model, and transactional outbox flow.
  2. **Live Demo (2–3 min)**: Running `docker compose up`, creating a customer and invoice, paying successfully (`tok_success`), paying with a decline (`tok_card_declined`), and observing webhook deliveries.
  3. **State Machine Walkthrough (1–2 min)**: Explaining states (`draft`, `open`, `paid`, `void`, `uncollectible`), terminal states, and transition rejection.
  4. **Failure-Mode Walkthrough (1–2 min)**: In-depth code walkthrough of Section 3 failure handling (concurrency row lock, timeout handling, or idempotency cache).

---

## Key Features

- **Integer Money Arithmetic**: All amounts are strictly modeled as integer minor units (`BIGINT` cents in USD). Zero floating-point numbers in the financial path.
- **Pessimistic Concurrency Locking**: Uses PostgreSQL `SELECT ... FOR UPDATE` row locks to serialize concurrent payment attempts for the same invoice, guaranteeing zero double charges.
- **Idempotent Payments**: A required `Idempotency-Key`, scoped to the invoice payment operation, returns cached terminal responses and rejects conflicting bodies.
- **Safe Ambiguous-Failure Handling**: A timeout or transport failure leaves the invoice open but records an `unknown` attempt; callers retry only with the same idempotency key. An explicit mock-PSP HTTP 500 is a retryable failed attempt.
- **Decoupled Webhooks**: Transactional outbox, HMAC-SHA256 signatures, stable event IDs, leased multi-worker delivery, and exponential retry scheduling.
- **One-Command Setup**: Automatically spins up PostgreSQL, runs migrations, boots the mock PSP, and starts the API service with `docker compose up`.

---

## Quick Start (Docker Compose)

The entire system runs with a single command:

```bash
docker compose up --build
```

This will launch:
- **PostgreSQL Database** on port `5432` (with automated SQL migrations applied).
- **Mock Payment Service Provider (PSP)** on `http://localhost:8081`.
- **Dodo Invoice & Payment Service** on `http://localhost:8080`.

To verify health:
```bash
curl -i http://localhost:8080/health
```

---

## Authentication

All API endpoints under `/v1/*` require an API key:
```http
Authorization: Bearer dp_live_testkey123
```
*Note: A default test business with API key `dp_live_testkey123` is automatically seeded into the database upon migration.*

---

## API End-to-End Walkthrough (cURL Examples)

### 1. Create a Customer
```bash
curl -i -X POST http://localhost:8080/v1/customers \
  -H "Authorization: Bearer dp_live_testkey123" \
  -H "Content-Type: application/json" \
  -d '{
    "name": "Jane Doe",
    "email": "jane@example.com"
  }'
```
*Save the returned `"id"` (e.g. `CUSTOMER_ID="<id>"`).*

---

### 2. Create an Invoice with Line Items
The server automatically computes item totals and invoice total in integer cents. Client totals are never trusted.

```bash
curl -i -X POST http://localhost:8080/v1/invoices \
  -H "Authorization: Bearer dp_live_testkey123" \
  -H "Content-Type: application/json" \
  -d '{
    "customer_id": "CUSTOMER_ID_HERE",
    "due_date": "2027-12-31T00:00:00Z",
    "auto_open": true,
    "line_items": [
      {
        "description": "Standard SaaS Subscription",
        "quantity": 2,
        "unit_amount_cents": 2500
      },
      {
        "description": "Setup & Onboarding",
        "quantity": 1,
        "unit_amount_cents": 1000
      }
    ]
  }'
```
*Notice: `total_amount_cents` is computed as `6000` ($60.00) and status is `open`.*

---

### 3. Register a Webhook Endpoint
Register an endpoint to receive cryptographically signed event notifications (`invoice.created`, `invoice.paid`, `invoice.payment_failed`):

```bash
curl -i -X POST http://localhost:8080/v1/webhooks/endpoints \
  -H "Authorization: Bearer dp_live_testkey123" \
  -H "Content-Type: application/json" \
  -d '{
    "url": "https://webhook.site/your-custom-uuid"
  }'
```

---

### 4. Attempt a Successful Payment (`tok_success`)
Pay the invoice using `tok_success`. We pass an `Idempotency-Key` header:

```bash
curl -i -X POST http://localhost:8080/v1/invoices/INVOICE_ID_HERE/pay \
  -H "Authorization: Bearer dp_live_testkey123" \
  -H "Idempotency-Key: idemp_sample_key_001" \
  -H "Content-Type: application/json" \
  -d '{
    "token": "tok_success"
  }'
```
*Response (`200 OK`):*
```json
{
  "invoice_id": "...",
  "payment_attempt_id": "...",
  "status": "succeeded",
  "invoice_status": "paid",
  "psp_reference": "...",
  "error_code": null,
  "message": "Payment successfully processed"
}
```

*Retrying with the same idempotency key returns the identical cached terminal response without another PSP charge. After a timeout or transport failure, retry only with that same key because the upstream outcome is ambiguous.*

---

### 5. Attempt a Failed Payment (`tok_card_declined`)
Create a new invoice and attempt payment with `tok_card_declined`:

```bash
curl -i -X POST http://localhost:8080/v1/invoices/NEW_INVOICE_ID/pay \
  -H "Authorization: Bearer dp_live_testkey123" \
  -H "Idempotency-Key: idemp_decline_001" \
  -H "Content-Type: application/json" \
  -d '{
    "token": "tok_card_declined"
  }'
```
*Response (`402 Payment Required`):*
The payment attempt records `card_declined`, and the invoice safely remains in the `open` state so the customer can retry with an alternative card.

---

### 6. Reconcile Webhook Events
Merchants can poll the event audit log at any time to reconcile missed or dropped deliveries:

```bash
curl -i -X GET "http://localhost:8080/v1/webhooks/events?limit=10" \
  -H "Authorization: Bearer dp_live_testkey123"
```

---

## Running Integration Tests

The test suite includes the three required integration tests:
1. **Concurrency Test (`tests/concurrency_test.rs`)**: Fires $N=10$ concurrent `POST /pay` requests at the exact same instant for the same invoice, asserting that at most one succeeds, $N-1$ are rejected, zero double charges occur, and database state is consistent.
2. **Idempotency Test (`tests/idempotency_test.rs`)**: Retries payment with the same idempotency key and asserts identical response caching and zero duplicate PSP calls.
3. **PSP Failure Test (`tests/psp_failure_test.rs`)**: Simulates slow downstream dependencies and asserts that their outcome is persisted as unknown rather than incorrectly treated as safely failed.

To run tests against your local database:
```bash
export DATABASE_URL="postgres://postgres:postgres@localhost:5432/dodo_payments"
cargo test
```

---

## Deliverables Index

- [`DESIGN.md`](./DESIGN.md) - Deep architectural breakdown: Data Model, State Machine, Concurrency & Failure Modes, Webhooks, API Key Model, Scope Decisions, and Production Readiness.
- [`AI_USAGE.md`](./AI_USAGE.md) - Mandatory disclosure of AI tool usage, 3 independent architectural decisions made against AI suggestions, and 1 error corrected.
- [`API.md`](./API.md) - API endpoints, request/response schemas, and standard error format.
- [`migrations/`](./migrations/) - SQL schema migrations.
- [`src/bin/mock_psp.rs`](./src/bin/mock_psp.rs) - Standalone Mock PSP service simulating card tokens.
