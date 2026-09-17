use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};
use sqlx::PgPool;
use tracing::warn;

use crate::errors::AppError;
use crate::models::business::Business;

pub async fn require_api_key(
    State(pool): State<PgPool>,
    mut req: Request,
    next: Next,
) -> Result<Response, AppError> {
    let auth_header = req
        .headers()
        .get("Authorization")
        .and_then(|h| h.to_str().ok());

    let token = match auth_header {
        Some(header) if header.starts_with("Bearer ") => header[7..].trim(),
        _ => {
            warn!("Missing or malformed Authorization Bearer header");
            return Err(AppError::Unauthorized);
        }
    };

    let key_hash = Business::hash_api_key(token);

    let business =
        sqlx::query_as::<_, Business>("SELECT * FROM businesses WHERE api_key_hash = $1")
            .bind(&key_hash)
            .fetch_optional(&pool)
            .await?;

    match business {
        Some(biz) => {
            req.extensions_mut().insert(biz);
            Ok(next.run(req).await)
        }
        None => {
            warn!("Invalid API key provided");
            Err(AppError::Unauthorized)
        }
    }
}
