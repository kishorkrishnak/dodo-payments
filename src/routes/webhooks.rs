use axum::{
    extract::{Extension, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::net::{IpAddr, Ipv4Addr};
use tokio::net::lookup_host;
use url::Url;
use uuid::Uuid;

use crate::errors::AppError;
use crate::models::business::Business;
use crate::models::webhook::{
    RegisterWebhookRequest, WebhookEndpoint, WebhookEndpointResponse, WebhookEvent,
};

#[derive(Debug, Deserialize)]
pub struct ListEventsQuery {
    pub since: Option<DateTime<Utc>>,
    pub limit: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct WebhookEventResponse {
    pub id: Uuid,
    pub business_id: Uuid,
    pub event_type: String,
    pub payload: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

pub async fn register_endpoint(
    State(pool): State<PgPool>,
    Extension(business): Extension<Business>,
    Json(req): Json<RegisterWebhookRequest>,
) -> Result<impl IntoResponse, AppError> {
    let url = validate_webhook_url(req.url.trim()).await?;

    // Generate a secure signing secret for HMAC verification
    let secret = format!("whsec_{}", Uuid::new_v4().simple());

    let endpoint = sqlx::query_as::<_, WebhookEndpoint>(
        r#"
        INSERT INTO webhook_endpoints (business_id, url, secret)
        VALUES ($1, $2, $3)
        RETURNING *
        "#,
    )
    .bind(business.id)
    .bind(url.as_str())
    .bind(secret)
    .fetch_one(&pool)
    .await?;

    let resp = WebhookEndpointResponse {
        id: endpoint.id,
        business_id: endpoint.business_id,
        url: endpoint.url,
        secret: endpoint.secret,
        is_active: endpoint.is_active,
        created_at: endpoint.created_at,
    };

    Ok((StatusCode::CREATED, Json(resp)))
}

async fn validate_webhook_url(raw_url: &str) -> Result<Url, AppError> {
    let url = Url::parse(raw_url).map_err(|_| {
        AppError::BadRequest("Webhook URL must be a valid absolute HTTPS URL".to_string())
    })?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(AppError::BadRequest(
            "Webhook URL must be an HTTPS URL without embedded credentials".to_string(),
        ));
    }

    let host = url.host_str().expect("checked above");
    let addresses: Vec<IpAddr> = lookup_host((host, url.port_or_known_default().unwrap_or(443)))
        .await
        .map_err(|_| AppError::BadRequest("Webhook host could not be resolved".to_string()))?
        .map(|address| address.ip())
        .collect();
    if addresses.is_empty()
        || addresses
            .iter()
            .any(|address| is_non_public_address(*address))
    {
        return Err(AppError::BadRequest(
            "Webhook URL must resolve exclusively to public addresses".to_string(),
        ));
    }
    Ok(url)
}

fn is_non_public_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(ip) => {
            ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_broadcast()
                || ip.is_documentation()
                || ip.is_unspecified()
                || ip.octets()[0] == 0
                || ip.octets()[0] >= 224
                || ip == Ipv4Addr::new(169, 254, 169, 254)
        }
        IpAddr::V6(ip) => {
            ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_multicast()
                || (ip.segments()[0] & 0xfe00) == 0xfc00 // Unique local fc00::/7
                || (ip.segments()[0] & 0xffc0) == 0xfe80 // Link-local fe80::/10
        }
    }
}

// Reconciliation endpoint: Allows businesses to reconcile any missed webhook deliveries
pub async fn list_events(
    State(pool): State<PgPool>,
    Extension(business): Extension<Business>,
    Query(query): Query<ListEventsQuery>,
) -> Result<impl IntoResponse, AppError> {
    let limit = query.limit.unwrap_or(50).clamp(1, 100);

    let events = if let Some(since) = query.since {
        sqlx::query_as::<_, WebhookEvent>(
            r#"
            SELECT * FROM webhook_events 
            WHERE business_id = $1 AND created_at >= $2 
            ORDER BY created_at ASC 
            LIMIT $3
            "#,
        )
        .bind(business.id)
        .bind(since)
        .bind(limit)
        .fetch_all(&pool)
        .await?
    } else {
        sqlx::query_as::<_, WebhookEvent>(
            r#"
            SELECT * FROM webhook_events 
            WHERE business_id = $1 
            ORDER BY created_at DESC 
            LIMIT $2
            "#,
        )
        .bind(business.id)
        .bind(limit)
        .fetch_all(&pool)
        .await?
    };

    let resp: Vec<WebhookEventResponse> = events
        .into_iter()
        .map(|e| WebhookEventResponse {
            id: e.id,
            business_id: e.business_id,
            event_type: e.event_type,
            payload: e.payload,
            created_at: e.created_at,
        })
        .collect();

    Ok((StatusCode::OK, Json(resp)))
}
