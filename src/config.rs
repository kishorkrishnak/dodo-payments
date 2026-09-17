use std::env;

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub database_url: String,
    pub server_port: u16,
    pub psp_base_url: String,
    pub psp_timeout_secs: u64,
}

impl AppConfig {
    pub fn from_env() -> Self {
        dotenvy::dotenv().ok();

        let database_url = env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgres@localhost:5432/dodo_payments".to_string()
        });

        let server_port = env::var("PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(8080);

        let psp_base_url =
            env::var("PSP_BASE_URL").unwrap_or_else(|_| "http://localhost:8081".to_string());

        let psp_timeout_secs = env::var("PSP_TIMEOUT_SECS")
            .ok()
            .and_then(|t| t.parse().ok())
            .unwrap_or(5);

        Self {
            database_url,
            server_port,
            psp_base_url,
            psp_timeout_secs,
        }
    }
}
