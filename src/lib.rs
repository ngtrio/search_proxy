pub mod admin;
pub mod auth;
pub mod catalog;
pub mod config;
pub mod db;
pub mod mcp;
pub mod metrics;
pub mod provider;
mod router;

use std::sync::Arc;

use axum::{Router, routing::get};
use db::Database;
use provider::ProviderManager;

#[derive(Clone, Debug)]
pub struct AdminConfig {
    pub cors_origins: Vec<String>,
    pub secure_cookies: bool,
}

impl Default for AdminConfig {
    fn default() -> Self {
        Self {
            cors_origins: Vec::new(),
            secure_cookies: true,
        }
    }
}

#[derive(Clone, Debug)]
pub struct McpConfig {
    pub allowed_hosts: Vec<String>,
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            allowed_hosts: vec!["localhost".into(), "127.0.0.1".into(), "::1".into()],
        }
    }
}

#[derive(Clone)]
pub struct AppState {
    pub db: Database,
    pub providers: Arc<ProviderManager>,
    pub admin: AdminConfig,
    pub mcp: McpConfig,
}

pub fn app(state: AppState) -> Router {
    let api_routes = Router::new()
        .route("/metrics", get(metrics::handler))
        .merge(admin::routes())
        .layer(admin::cors_layer(&state.admin));

    Router::new()
        .route("/health/live", get(|| async { "ok" }))
        .nest("/mcp", mcp::routes(state.clone()))
        .nest("/api", api_routes)
        .with_state(state)
}
