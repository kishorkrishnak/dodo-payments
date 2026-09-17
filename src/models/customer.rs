use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Customer {
    pub id: Uuid,
    pub business_id: Uuid,
    pub name: String,
    pub email: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct CreateCustomerRequest {
    pub name: String,
    pub email: String,
}

#[derive(Debug, Serialize)]
pub struct CustomerResponse {
    pub id: Uuid,
    pub business_id: Uuid,
    pub name: String,
    pub email: String,
    pub created_at: DateTime<Utc>,
}

impl From<Customer> for CustomerResponse {
    fn from(c: Customer) -> Self {
        Self {
            id: c.id,
            business_id: c.business_id,
            name: c.name,
            email: c.email,
            created_at: c.created_at,
        }
    }
}
