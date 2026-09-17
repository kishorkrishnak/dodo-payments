use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use crate::errors::AppError;

// We model 5 explicit lifecycle states:
// - Draft: Created before it is finalized for collection.
// - Open: Finalized invoice awaiting payment. Only Open invoices accept payments.
// - Paid: Terminal state achieved upon successful payment settlement.
// - Void: Terminal state when merchant cancels an invoice.
// - Uncollectible: Terminal state when merchant designates invoice as bad debt.
// Reversibility: Paid, Void, and Uncollectible are strictly terminal to preserve financial auditability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "VARCHAR", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum InvoiceStatus {
    Draft,
    Open,
    Paid,
    Void,
    Uncollectible,
}

impl InvoiceStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            InvoiceStatus::Draft => "draft",
            InvoiceStatus::Open => "open",
            InvoiceStatus::Paid => "paid",
            InvoiceStatus::Void => "void",
            InvoiceStatus::Uncollectible => "uncollectible",
        }
    }

    pub fn from_str_strict(s: &str) -> Result<Self, AppError> {
        match s.to_lowercase().as_str() {
            "draft" => Ok(InvoiceStatus::Draft),
            "open" => Ok(InvoiceStatus::Open),
            "paid" => Ok(InvoiceStatus::Paid),
            "void" => Ok(InvoiceStatus::Void),
            "uncollectible" => Ok(InvoiceStatus::Uncollectible),
            _ => Err(AppError::BadRequest(format!(
                "Invalid invoice status: '{}'",
                s
            ))),
        }
    }

    // Strict transition validation matrix:
    // - Draft -> Open (finalize)
    // - Draft -> Void (cancel before issuing)
    // - Open -> Paid (payment success)
    // - Open -> Void (merchant cancel)
    // - Open -> Uncollectible (bad debt)
    // All other transitions, including transitions out of Paid, Void, and Uncollectible, are rejected.
    pub fn can_transition_to(&self, target: InvoiceStatus) -> bool {
        matches!(
            (self, target),
            (InvoiceStatus::Draft, InvoiceStatus::Open)
                | (InvoiceStatus::Draft, InvoiceStatus::Void)
                | (InvoiceStatus::Open, InvoiceStatus::Paid)
                | (InvoiceStatus::Open, InvoiceStatus::Void)
                | (InvoiceStatus::Open, InvoiceStatus::Uncollectible)
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Invoice {
    pub id: Uuid,
    pub business_id: Uuid,
    pub customer_id: Uuid,
    pub status: String,
    pub currency: String,
    // Integer minor units (cents). Guaranteed no floating-point arithmetic.
    pub total_amount_cents: i64,
    pub due_date: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct InvoiceLineItem {
    pub id: Uuid,
    pub invoice_id: Uuid,
    pub description: String,
    pub quantity: i64,
    pub unit_amount_cents: i64,
    pub total_amount_cents: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct CreateLineItemRequest {
    pub description: String,
    pub quantity: i64,
    pub unit_amount_cents: i64,
}

#[derive(Debug, Deserialize)]
pub struct CreateInvoiceRequest {
    pub customer_id: Uuid,
    pub due_date: DateTime<Utc>,
    pub line_items: Vec<CreateLineItemRequest>,
    pub auto_open: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct LineItemResponse {
    pub id: Uuid,
    pub description: String,
    pub quantity: i64,
    pub unit_amount_cents: i64,
    pub total_amount_cents: i64,
}

#[derive(Debug, Serialize)]
pub struct InvoiceResponse {
    pub id: Uuid,
    pub business_id: Uuid,
    pub customer_id: Uuid,
    pub status: String,
    pub currency: String,
    pub total_amount_cents: i64,
    pub due_date: DateTime<Utc>,
    pub line_items: Vec<LineItemResponse>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
