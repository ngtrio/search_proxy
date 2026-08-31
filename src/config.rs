use std::{
    collections::HashSet,
    env,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
};

use anyhow::{Context, ensure};
use axum::http::HeaderValue;

pub struct Config {
    pub bind: SocketAddr,
    pub database_url: String,
    pub admin_username: String,
    pub admin_password: Option<String>,
    pub admin_cors_origins: Vec<String>,
    pub admin_cookie_secure: bool,
    pub mcp_allowed_hosts: Vec<String>,
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
            mcp_allowed_hosts: parse_allowed_hosts(
                &env::var("MCP_ALLOWED_HOSTS").unwrap_or_else(|_| "localhost,127.0.0.1,::1".into()),
            )?,
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

fn parse_allowed_hosts(raw: &str) -> anyhow::Result<Vec<String>> {
    let mut seen = HashSet::new();
    let mut hosts = Vec::new();
    for host in raw
        .split(',')
        .map(str::trim)
        .filter(|host| !host.is_empty())
    {
        ensure!(host != "*", "MCP_ALLOWED_HOSTS cannot contain '*'");
        let valid = host.parse::<IpAddr>().is_ok()
            || host
                .parse::<axum::http::uri::Authority>()
                .is_ok_and(|authority| !authority.host().is_empty());
        ensure!(
            valid,
            "MCP_ALLOWED_HOSTS contains an invalid host or host:port authority: {host}"
        );
        if seen.insert(host.to_ascii_lowercase()) {
            hosts.push(host.to_owned());
        }
    }
    ensure!(
        !hosts.is_empty(),
        "MCP_ALLOWED_HOSTS must contain at least one host"
    );
    Ok(hosts)
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

#[cfg(test)]
mod tests {
    use super::parse_allowed_hosts;

    #[test]
    fn parses_and_deduplicates_mcp_allowed_hosts() {
        assert_eq!(
            parse_allowed_hosts("localhost, mcp.example.com, mcp.example.com:8443, ::1").unwrap(),
            [
                "localhost",
                "mcp.example.com",
                "mcp.example.com:8443",
                "::1"
            ]
        );
    }

    #[test]
    fn rejects_an_empty_or_wildcard_mcp_allowed_host_list() {
        assert!(parse_allowed_hosts("").is_err());
        assert!(parse_allowed_hosts("localhost,*").is_err());
        assert!(parse_allowed_hosts("https://mcp.example.com").is_err());
    }
}
