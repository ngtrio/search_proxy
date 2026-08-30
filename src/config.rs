use std::{env, net::SocketAddr, path::PathBuf};

use anyhow::{Context, ensure};
use axum::http::HeaderValue;

pub struct Config {
    pub bind: SocketAddr,
    pub database_url: String,
    pub admin_username: String,
    pub admin_password: Option<String>,
    pub admin_cors_origins: Vec<String>,
    pub admin_cookie_secure: bool,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let admin_cookie_secure = env::var("ADMIN_COOKIE_SECURE")
            .unwrap_or_else(|_| "true".into())
            .parse::<bool>()
            .context("ADMIN_COOKIE_SECURE must be true or false")?;

        Ok(Self {
            bind: env::var("GATEWAY_BIND")
                .unwrap_or_else(|_| "0.0.0.0:3000".into())
                .parse()?,
            database_url: env::var("DATABASE_URL")
                .unwrap_or_else(|_| "sqlite://data/gateway.db?mode=rwc".into()),
            admin_username: env::var("ADMIN_USERNAME").unwrap_or_else(|_| "admin".into()),
            admin_password: env::var("ADMIN_PASSWORD").ok(),
            admin_cors_origins: parse_cors_origins()?,
            admin_cookie_secure,
        })
    }

    pub fn ensure_data_dir(&self) -> anyhow::Result<()> {
        let path = self.database_url.trim_start_matches("sqlite://");
        let path = path.split('?').next().unwrap_or(path);
        if let Some(parent) = PathBuf::from(path).parent() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(())
    }
}

fn parse_cors_origins() -> anyhow::Result<Vec<String>> {
    env::var("ADMIN_CORS_ORIGINS")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|origin| !origin.is_empty())
        .map(|origin| {
            ensure!(origin != "*", "ADMIN_CORS_ORIGINS cannot contain '*'");
            let parsed = url::Url::parse(origin)
                .with_context(|| format!("invalid CORS origin: {origin}"))?;
            ensure!(
                matches!(parsed.scheme(), "http" | "https"),
                "CORS origin must use http or https: {origin}"
            );
            ensure!(
                parsed.host_str().is_some()
                    && parsed.username().is_empty()
                    && parsed.password().is_none()
                    && (parsed.path().is_empty() || parsed.path() == "/")
                    && parsed.query().is_none()
                    && parsed.fragment().is_none(),
                "CORS origin must contain only scheme, host, and optional port: {origin}"
            );
            let normalized = origin.strip_suffix('/').unwrap_or(origin).to_owned();
            HeaderValue::from_str(&normalized)
                .with_context(|| format!("invalid CORS origin header value: {origin}"))?;
            Ok(normalized)
        })
        .collect()
}
