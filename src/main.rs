use axum::{
    extract::FromRef,
    middleware,
    routing::{get, post},
    Router,
};
use sqlx::PgPool;
use std::net::SocketAddr;
use std::sync::Arc;
use tower_http::trace::TraceLayer;
use tracing::info;

use dodo_payments::config::AppConfig;
use dodo_payments::db;
use dodo_payments::routes::{
    auth::require_api_key,
    customers::{create_customer, get_customer, list_customers},
    health::health_check,
    invoices::{
        create_invoice, finalize_invoice, get_invoice, list_invoices, mark_uncollectible,
        pay_invoice, void_invoice,
    },
    webhooks::{list_events, register_endpoint},
};
use dodo_payments::services::{
    invoice_service::InvoiceService, psp_client::PspClient, webhook_worker::WebhookWorker,
};

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub invoice_service: Arc<InvoiceService>,
}

impl FromRef<AppState> for PgPool {
    fn from_ref(state: &AppState) -> Self {
        state.pool.clone()
    }
}

impl FromRef<AppState> for Arc<InvoiceService> {
    fn from_ref(state: &AppState) -> Self {
        state.invoice_service.clone()
    }
}

pub fn create_router(app_state: AppState) -> Router {
    let api_routes = Router::new()
        .route("/customers", post(create_customer).get(list_customers))
        .route("/customers/{id}", get(get_customer))
        .route("/invoices", post(create_invoice).get(list_invoices))
        .route("/invoices/{id}", get(get_invoice))
        .route("/invoices/{id}/finalize", post(finalize_invoice))
        .route("/invoices/{id}/void", post(void_invoice))
        .route(
            "/invoices/{id}/mark-uncollectible",
            post(mark_uncollectible),
        )
        .route("/invoices/{id}/pay", post(pay_invoice))
        .route("/webhooks/endpoints", post(register_endpoint))
        .route("/webhooks/events", get(list_events))
        .layer(middleware::from_fn_with_state(
            app_state.clone(),
            require_api_key,
        ));

    Router::new()
        .route("/health", get(health_check))
        .nest("/v1", api_routes)
        .layer(TraceLayer::new_for_http())
        .with_state(app_state)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let config = AppConfig::from_env();
    info!("Starting Dodo Invoice & Payment Service...");
    info!("Connecting to upstream PSP at: {}", config.psp_base_url);

    let pool = db::create_pool(&config.database_url).await?;
    db::run_migrations(&pool).await?;

    // Start decoupled background webhook worker
    let worker = WebhookWorker::new(pool.clone());
    tokio::spawn(async move {
        worker.run_loop().await;
    });

    let psp_client = PspClient::new(config.psp_base_url, config.psp_timeout_secs);
    let invoice_service = Arc::new(InvoiceService::new(pool.clone(), psp_client));

    let app_state = AppState {
        pool,
        invoice_service,
    };

    let app = create_router(app_state);

    let addr = SocketAddr::from(([0, 0, 0, 0], config.server_port));
    info!("Server listening on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
