pub mod admin;
pub mod auth;
pub mod catalog;
pub mod config;
pub mod db;
pub mod gateway;
pub mod mcp;
pub mod provider;
mod router;

use std::sync::Arc;

use axum::{Router, routing::get};
use db::Database;
use gateway::ToolGateway;
use provider::ProviderManager;

#[derive(Clone)]
pub struct AppState {
    pub db: Database,
    pub providers: Arc<ProviderManager>,
    pub gateway: Arc<ToolGateway>,
}

pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/health/live", get(|| async { "ok" }))
        .nest("/mcp", mcp::routes(state.clone()))
        .nest("/admin/api", admin::routes())
        .route("/admin", get(admin::spa))
        .route("/admin/assets/{*path}", get(admin::asset))
        .route("/admin/{*path}", get(admin::spa))
        .with_state(state)
}
