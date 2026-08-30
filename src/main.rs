use std::sync::Arc;
use tavily_mcp_gateway::{
    AppState, admin, app, config::Config, db::Database, gateway::ToolGateway,
    provider::ProviderManager,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .json()
        // Keep dependency transports from logging MCP payloads or credentials.
        .with_env_filter(tracing_subscriber::EnvFilter::new(
            "tavily_mcp_gateway=info",
        ))
        .init();
    let config = Config::from_env()?;
    config.ensure_data_dir()?;
    let db = Database::connect(&config.database_url).await?;
    admin::bootstrap(
        &db,
        &config.admin_username,
        config.admin_password.as_deref(),
    )
    .await?;
    let providers = Arc::new(ProviderManager::new(db.clone()).await?);
    let gateway = Arc::new(ToolGateway::new(db.clone(), Arc::clone(&providers)));
    let state = AppState {
        db,
        providers,
        gateway,
    };
    let listener = tokio::net::TcpListener::bind(config.bind).await?;
    tracing::info!(bind=%config.bind,"gateway listening");
    axum::serve(listener, app(state)).await?;
    Ok(())
}
