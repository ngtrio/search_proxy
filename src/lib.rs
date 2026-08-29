pub mod admin;
pub mod auth;
pub mod config;
pub mod db;
pub mod mcp;
pub mod provider;
pub mod router;

use std::sync::Arc;

use axum::{Router, routing::get};
use db::Database;
use provider::ProviderRegistry;

#[derive(Clone)]
pub struct AppState {
    pub db: Database,
    pub providers: Arc<ProviderRegistry>,
    pub config: config::Config,
}

pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/health/live", get(|| async { "ok" }))
        .route("/health/ready", get(admin::ready))
        .nest("/mcp", mcp::routes(state.clone()))
        .nest("/admin/api", admin::routes())
        .route("/admin", get(admin::spa))
        .route("/admin/assets/{*path}", get(admin::asset))
        .route("/admin/{*path}", get(admin::spa))
        .with_state(state)
}
