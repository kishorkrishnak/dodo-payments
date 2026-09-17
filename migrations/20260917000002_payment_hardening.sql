-- Preserve safe retry state for ambiguous downstream outcomes and scope idempotency to an operation.
ALTER TABLE idempotency_records
    ADD COLUMN IF NOT EXISTS payment_attempt_id UUID NULL REFERENCES payment_attempts(id) ON DELETE SET NULL;

ALTER TABLE payment_attempts
    RENAME COLUMN card_token TO payment_method_reference;

ALTER TABLE idempotency_records
    DROP CONSTRAINT IF EXISTS uq_idempotency_business_key;

CREATE UNIQUE INDEX IF NOT EXISTS uq_idempotency_business_path_key
    ON idempotency_records (business_id, request_path, idempotency_key);

ALTER TABLE webhook_deliveries
    ADD COLUMN IF NOT EXISTS lease_expires_at TIMESTAMPTZ NULL;

ALTER TABLE invoices
    ADD CONSTRAINT invoices_status_check
    CHECK (status IN ('draft', 'open', 'paid', 'void', 'uncollectible')),
    ADD CONSTRAINT invoices_currency_check CHECK (currency = 'USD'),
    ADD CONSTRAINT invoices_total_nonnegative_check CHECK (total_amount_cents >= 0);

ALTER TABLE invoice_line_items
    ADD CONSTRAINT invoice_line_items_quantity_positive_check CHECK (quantity > 0),
    ADD CONSTRAINT invoice_line_items_amount_nonnegative_check CHECK (unit_amount_cents >= 0),
    ADD CONSTRAINT invoice_line_items_total_nonnegative_check CHECK (total_amount_cents >= 0);

ALTER TABLE payment_attempts
    ADD CONSTRAINT payment_attempts_status_check CHECK (status IN ('pending', 'unknown', 'succeeded', 'failed')),
    ADD CONSTRAINT payment_attempts_amount_nonnegative_check CHECK (amount_cents >= 0);
