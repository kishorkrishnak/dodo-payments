use axum::{
    extract::{Extension, Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use sqlx::PgPool;
use uuid::Uuid;

use crate::errors::AppError;
use crate::models::business::Business;
use crate::models::customer::{CreateCustomerRequest, Customer, CustomerResponse};

pub async fn create_customer(
    State(pool): State<PgPool>,
    Extension(business): Extension<Business>,
    Json(req): Json<CreateCustomerRequest>,
) -> Result<impl IntoResponse, AppError> {
    if req.name.trim().is_empty()
        || req.name.len() > 255
        || req.email.trim().is_empty()
        || req.email.len() > 255
        || !req.email.contains('@')
    {
        return Err(AppError::BadRequest(
            "Customer name must be 1–255 characters and email must be a plausible address"
                .to_string(),
        ));
    }

    let customer = sqlx::query_as::<_, Customer>(
        r#"
        INSERT INTO customers (business_id, name, email)
        VALUES ($1, $2, $3)
        RETURNING *
        "#,
    )
    .bind(business.id)
    .bind(req.name.trim())
    .bind(req.email.trim())
    .fetch_one(&pool)
    .await?;

    let resp = CustomerResponse::from(customer);
    Ok((StatusCode::CREATED, Json(resp)))
}

pub async fn get_customer(
    State(pool): State<PgPool>,
    Extension(business): Extension<Business>,
    Path(customer_id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let customer =
        sqlx::query_as::<_, Customer>("SELECT * FROM customers WHERE id = $1 AND business_id = $2")
            .bind(customer_id)
            .bind(business.id)
            .fetch_optional(&pool)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("Customer {} not found", customer_id)))?;

    Ok((StatusCode::OK, Json(CustomerResponse::from(customer))))
}

pub async fn list_customers(
    State(pool): State<PgPool>,
    Extension(business): Extension<Business>,
) -> Result<impl IntoResponse, AppError> {
    let customers = sqlx::query_as::<_, Customer>(
        "SELECT * FROM customers WHERE business_id = $1 ORDER BY created_at DESC",
    )
    .bind(business.id)
    .fetch_all(&pool)
    .await?;

    let resp: Vec<CustomerResponse> = customers.into_iter().map(CustomerResponse::from).collect();
    Ok((StatusCode::OK, Json(resp)))
}
