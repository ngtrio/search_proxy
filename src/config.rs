use std::{env, net::SocketAddr, path::PathBuf};

#[derive(Clone)]
pub struct Config {
    pub bind: SocketAddr,
    pub database_url: String,
    pub admin_username: String,
    pub admin_password: Option<String>,
    pub production: bool,
    pub public_origin: Option<String>,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            bind: env::var("GATEWAY_BIND")
                .unwrap_or_else(|_| "0.0.0.0:3000".into())
                .parse()?,
            database_url: env::var("DATABASE_URL")
                .unwrap_or_else(|_| "sqlite://data/gateway.db?mode=rwc".into()),
            admin_username: env::var("ADMIN_USERNAME").unwrap_or_else(|_| "admin".into()),
            admin_password: env::var("ADMIN_PASSWORD").ok(),
            production: env::var("GATEWAY_ENV").is_ok_and(|v| v == "production"),
            public_origin: env::var("PUBLIC_ORIGIN").ok(),
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
