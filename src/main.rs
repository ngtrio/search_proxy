use std::sync::Arc;
use tavily_mcp_gateway::{
    AppState, admin, app, config::Config, db::Database, provider::ProviderRegistry,
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
    let providers = Arc::new(ProviderRegistry::new(db.clone()).await?);
    let state = AppState {
        db,
        providers,
        config: config.clone(),
    };
    admin::bootstrap(&state).await?;
    let listener = tokio::net::TcpListener::bind(config.bind).await?;
    tracing::info!(bind=%config.bind,"gateway listening");
    axum::serve(listener, app(state)).await?;
    Ok(())
}
