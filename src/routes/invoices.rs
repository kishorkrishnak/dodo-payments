use axum::{
    body::Bytes,
    extract::{Extension, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use std::sync::Arc;
use uuid::Uuid;

use crate::errors::AppError;
use crate::models::business::Business;
use crate::models::invoice::{CreateInvoiceRequest, InvoiceStatus};
use crate::models::payment::PayInvoiceRequest;
use crate::services::invoice_service::InvoiceService;

#[derive(Debug, Deserialize)]
pub struct ListInvoicesQuery {
    pub status: Option<String>,
}

pub async fn create_invoice(
    State(service): State<Arc<InvoiceService>>,
    Extension(business): Extension<Business>,
    Json(req): Json<CreateInvoiceRequest>,
) -> Result<impl IntoResponse, AppError> {
    let invoice = service.create_invoice(business.id, req).await?;
    Ok((StatusCode::CREATED, Json(invoice)))
}

pub async fn get_invoice(
    State(service): State<Arc<InvoiceService>>,
    Extension(business): Extension<Business>,
    Path(invoice_id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let invoice = service.get_invoice(business.id, invoice_id).await?;
    Ok((StatusCode::OK, Json(invoice)))
}

pub async fn finalize_invoice(
    State(service): State<Arc<InvoiceService>>,
    Extension(business): Extension<Business>,
    Path(invoice_id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let invoice = service
        .transition_invoice(business.id, invoice_id, InvoiceStatus::Open)
        .await?;
    Ok((StatusCode::OK, Json(invoice)))
}

pub async fn void_invoice(
    State(service): State<Arc<InvoiceService>>,
    Extension(business): Extension<Business>,
    Path(invoice_id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let invoice = service
        .transition_invoice(business.id, invoice_id, InvoiceStatus::Void)
        .await?;
    Ok((StatusCode::OK, Json(invoice)))
}

pub async fn mark_uncollectible(
    State(service): State<Arc<InvoiceService>>,
    Extension(business): Extension<Business>,
    Path(invoice_id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let invoice = service
        .transition_invoice(business.id, invoice_id, InvoiceStatus::Uncollectible)
        .await?;
    Ok((StatusCode::OK, Json(invoice)))
}

pub async fn list_invoices(
    State(service): State<Arc<InvoiceService>>,
    Extension(business): Extension<Business>,
    Query(query): Query<ListInvoicesQuery>,
) -> Result<impl IntoResponse, AppError> {
    let invoices = service.list_invoices(business.id, query.status).await?;
    Ok((StatusCode::OK, Json(invoices)))
}

pub async fn pay_invoice(
    State(service): State<Arc<InvoiceService>>,
    Extension(business): Extension<Business>,
    Path(invoice_id): Path<Uuid>,
    headers: HeaderMap,
    body_bytes: Bytes,
) -> Result<impl IntoResponse, AppError> {
    let idempotency_key = headers
        .get("Idempotency-Key")
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string());

    let raw_body = std::str::from_utf8(&body_bytes)
        .map_err(|_| AppError::BadRequest("Invalid UTF-8 payload".to_string()))?;

    let req: PayInvoiceRequest = serde_json::from_str(raw_body)
        .map_err(|e| AppError::BadRequest(format!("Malformed JSON payload: {}", e)))?;

    let resp = service
        .pay_invoice(business.id, invoice_id, req, idempotency_key, raw_body)
        .await?;

    let status = if resp.status == "failed" {
        StatusCode::PAYMENT_REQUIRED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(resp)))
}
