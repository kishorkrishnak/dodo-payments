# AI Usage Disclosure

## Tool Used

I used **OpenAI Codex** as a pair-programming and code-review assistant.

It helped me review the Rust, database, concurrency, and webhook design; identify edge cases around payment idempotency and uncertain PSP outcomes; refine migrations, tests, documentation, and Docker verification; and explain design decisions so that I could review and understand the final implementation.

I reviewed the resulting code and kept the final design intentionally scoped to the assignment.

## Three Decisions I Made

### PostgreSQL Transactional Outbox Rather Than a Message Broker

**AI input:** Codex identified an external queue or message broker as a possible way to decouple webhook delivery at larger scale.

**What I chose:** Webhook events and delivery records are written in the same PostgreSQL transaction as the invoice or payment state change.

**Why:** Kafka, RabbitMQ, or Redis would add infrastructure and a distributed dual-write problem that is not justified for this assignment. The PostgreSQL transactional outbox keeps the business-state update and the obligation to deliver its webhook atomic. A background worker then sends deliveries asynchronously and retries failures.

### Lock the Invoice Before Contacting the PSP

**AI input:** Codex discussed optimistic concurrency with a version field as an alternative.

**What I chose:** A payment request locks the invoice row before contacting the PSP.

**Why:** Optimistic concurrency alone could allow two requests to both reach the PSP before one loses a database version race. That is unacceptable for a payment flow: the key invariant is that one invoice must not be charged twice. Serializing payment requests for an invoice before the external call prioritizes that invariant. The database-backed concurrency test verifies that only one payment attempt can succeed and that the final invoice state remains consistent.

### Preserve Uncertainty After a PSP Timeout

**AI input:** Codex described a more complete production design with a `paying` or `pending_settlement` state, provider-side payment intents, and a reconciliation worker.

**What I chose:** A timeout records an `unknown` payment attempt and leaves the invoice payable, but rejects a fresh payment key until the original attempt has been resolved.

**Why:** A PSP timeout does not prove that the payment was not charged, so retrying blindly could risk a second charge. A production integration would add provider webhooks and reconciliation, but the assignment's mock PSP does not provide an outcome-lookup API. I deliberately did not introduce unsupported reconciliation infrastructure.

## One AI-Assisted Implementation Issue I Had to Correct

During review, the payment flow had a transaction-boundary weakness: cleanup of an idempotency record could use a separate pool connection while the invoice transaction was still open. Under concurrent load, that could create avoidable lock or pool-pressure failures.

The cleanup was moved into the existing transaction and committed before returning the validation error. This keeps related changes atomic and avoids competing with the transaction that owns the invoice lock.
