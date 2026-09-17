# Dodo Payments - API Documentation

## Overview

The Dodo Payments Invoice & Payment Service provides API-key authenticated endpoints for managing customers, creating invoices with calculated line items, executing idempotent payment attempts against an external PSP, and receiving decoupled signed webhook notifications.

- **Base URL (Local)**: `http://localhost:8080/v1`
- **Mock PSP URL (Local)**: `http://localhost:8081`

---

## Authentication

All `/v1/*` requests require an API key passed via the HTTP `Authorization` header:

```http
Authorization: Bearer <api_key>
```

A default test business is pre-seeded in the database:
- **Test Key**: `dp_live_testkey123`
- **Prefix**: `dp_live_test`
- **SHA-256 Hash**: `eeca362a8fdcd19e044d59d2aed9c50b68bb33228ebcf2be0d59f0d9475ccdea`

---

## Standard Error Format

All error responses return standard JSON with RFC-compliant status codes:

```json
{
  "error": {
    "code": "invalid_state_transition",
    "message": "Cannot pay invoice: current state is 'paid'. Payments can only be processed on 'open' invoices."
  }
}
```

### Common Error Codes

| Status Code | Code String | Description |
|---|---|---|
| `401 Unauthorized` | `unauthorized` | Missing or invalid API key |
| `404 Not Found` | `not_found` | Resource (invoice, customer) does not exist |
| `422 Unprocessable Entity` | `invalid_state_transition` | Transition rejected by invoice state machine |
| `409 Conflict` | `idempotency_conflict` | Reused key with a differing body, a concurrent same-key request, or a separate key while a payment outcome is unresolved |
| `402 Payment Required` | `payment_failed` | Card declined or insufficient funds |
| `504 Gateway Timeout` | `psp_timeout` | Downstream PSP exceeded client timeout limit (5s) |
| `502 Bad Gateway` | `psp_network_error` | Downstream PSP returned a retryable 5xx response or the connection failed |
| `400 Bad Request` | `bad_request` | Invalid JSON syntax or semantic validation failure |
| `400 Bad Request` | `idempotency_key_required` | Missing, empty, or overlong payment idempotency key |

---

## Endpoints

### 1. Customers

#### `POST /v1/customers`
Create a customer scoped to the authenticated business.

**Request Body:**
```json
{
  "name": "Jane Doe",
  "email": "jane@example.com"
}
```

**Response (`201 Created`):**
```json
{
  "id": "1840e791-dc6a-4933-a36c-ae30773d2218",
  "business_id": "00000000-0000-0000-0000-000000000001",
  "name": "Jane Doe",
  "email": "jane@example.com",
  "created_at": "2026-09-17T12:00:00Z"
}
```

#### `GET /v1/customers/{id}`
Retrieve customer details by ID.

**Response (`200 OK`):** The same customer object returned by `POST /v1/customers`.

#### `GET /v1/customers`
List all customers belonging to the authenticated business.

**Response (`200 OK`):**
```json
[
  {
    "id": "1840e791-dc6a-4933-a36c-ae30773d2218",
    "business_id": "00000000-0000-0000-0000-000000000001",
    "name": "Jane Doe",
    "email": "jane@example.com",
    "created_at": "2026-09-17T12:00:00Z"
  }
]
```

---

### 2. Invoices

#### `POST /v1/invoices`
Creates an invoice. The server strictly computes line item totals and invoice total in **integer cents** (USD). Client totals are never trusted.

**Request Body:**
```json
{
  "customer_id": "1840e791-dc6a-4933-a36c-ae30773d2218",
  "due_date": "2027-12-31T00:00:00Z",
  "auto_open": true,
  "line_items": [
    {
      "description": "Standard SaaS Subscription",
      "quantity": 2,
      "unit_amount_cents": 2500
    },
    {
      "description": "Setup Fee",
      "quantity": 1,
      "unit_amount_cents": 1000
    }
  ]
}
```

**Response (`201 Created`):**
```json
{
  "id": "52f6ea60-b99b-4e1b-8be2-8b4d8d1e39a3",
  "business_id": "00000000-0000-0000-0000-000000000001",
  "customer_id": "1840e791-dc6a-4933-a36c-ae30773d2218",
  "status": "open",
  "currency": "USD",
  "total_amount_cents": 6000,
  "due_date": "2027-12-31T00:00:00Z",
  "line_items": [
    {
      "id": "76e33db3-8f6a-42c2-b5e1-5254dfb93e43",
      "description": "Standard SaaS Subscription",
      "quantity": 2,
      "unit_amount_cents": 2500,
      "total_amount_cents": 5000
    },
    {
      "id": "893c52a0-4286-4f81-a6ef-ecfbe7ec8821",
      "description": "Setup Fee",
      "quantity": 1,
      "unit_amount_cents": 1000,
      "total_amount_cents": 1000
    }
  ],
  "created_at": "2026-09-17T12:05:00Z",
  "updated_at": "2026-09-17T12:05:00Z"
}
```

#### `GET /v1/invoices/{id}`
Retrieve an invoice and its line items.

**Response (`200 OK`):** The same invoice object returned by `POST /v1/invoices`.

#### `GET /v1/invoices?status=open`
List invoices filterable by state (`draft`, `open`, `paid`, `void`, `uncollectible`).

**Response (`200 OK`):** An array of invoice objects in the shape returned by `POST /v1/invoices`.

#### `POST /v1/invoices/{id}/finalize`
Transitions a `draft` invoice to `open`.

**Response (`200 OK`):** The updated invoice object, with `status: "open"`.

#### `POST /v1/invoices/{id}/void`
Transitions a `draft` or `open` invoice to terminal `void`.

**Response (`200 OK`):** The updated invoice object, with `status: "void"`.

#### `POST /v1/invoices/{id}/mark-uncollectible`
Transitions an `open` invoice to terminal `uncollectible`.

**Response (`200 OK`):** The updated invoice object, with `status: "uncollectible"`.

---

### 3. Payment Processing

#### `POST /v1/invoices/{id}/pay`
Executes a payment attempt against the mock Payment Service Provider (PSP).

**Headers:**
- `Authorization: Bearer <api_key>` (Required)
- `Idempotency-Key: <unique_string>` (Required; maximum 255 characters)

**Request Body:**
```json
{
  "token": "tok_success"
}
```

**Supported Tokens:**
- `tok_success`: Simulates successful settlement. Transitions invoice to `paid`.
- `tok_card_declined`: Simulates card decline (`402 Payment Required`). Invoice remains `open`.
- `tok_insufficient_funds`: Simulates insufficient funds. Invoice remains `open`.
- `tok_timeout`: Mock sleeps 30s. Client timeout trips at 5s (`504 Gateway Timeout`). Invoice remains `open`, while the attempt is `unknown`; retry only with the same key.
- `tok_network_error`: The supplied mock returns HTTP 500 (`502 Bad Gateway`). It records a retryable failed attempt, leaves the invoice `open`, and permits a new key. A real transport drop is instead treated as `unknown` and must reuse its key.

**Successful Response (`200 OK`):**
```json
{
  "invoice_id": "52f6ea60-b99b-4e1b-8be2-8b4d8d1e39a3",
  "payment_attempt_id": "c71d6e15-88f5-4122-b5f7-64dfdf7000bf",
  "status": "succeeded",
  "invoice_status": "paid",
  "psp_reference": "098e9ad9-2bb2-466d-a129-8732e4dcbfa6",
  "error_code": null,
  "message": "Payment successfully processed"
}
```

---

### 4. Webhooks

#### `POST /v1/webhooks/endpoints`
Registers an HTTPS callback endpoint resolving to public addresses. Returns an auto-generated signing secret (`whsec_...`). Deliveries are at-least-once; consumers deduplicate with `X-Webhook-Event-Id`.

**Request Body:**
```json
{
  "url": "https://webhook.site/my-uuid"
}
```

**Response (`201 Created`):**
```json
{
  "id": "e98e4f50-cbdb-47d0-8f92-91185f39644b",
  "business_id": "00000000-0000-0000-0000-000000000001",
  "url": "https://webhook.site/my-uuid",
  "secret": "whsec_721868ef6bb64c7694f479a33bbbaef5",
  "is_active": true,
  "created_at": "2026-09-17T12:10:00Z"
}
```

#### `GET /v1/webhooks/events?since=2026-09-17T00:00:00Z&limit=50`
Audit log & reconciliation endpoint for missed or delayed deliveries.

`since` is optional; `limit` is optional and clamped to `1..100` (default `50`).

**Response (`200 OK`):**
```json
[
  {
    "id": "5a0d1616-b7b7-4b75-8b28-625cb0d00a46",
    "business_id": "00000000-0000-0000-0000-000000000001",
    "event_type": "invoice.paid",
    "payload": {
      "invoice_id": "52f6ea60-b99b-4e1b-8be2-8b4d8d1e39a3",
      "status": "paid"
    },
    "created_at": "2026-09-17T12:15:00Z"
  }
]
```
