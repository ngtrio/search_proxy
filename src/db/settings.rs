use serde::Serialize;

use super::Database;

const DEFAULT_TIMEOUT_SECONDS: i64 = 120;

#[derive(Debug, Clone, Serialize)]
pub struct GatewaySettings {
    pub default_timeout_seconds: i64,
}

impl Database {
    pub async fn settings(&self) -> Result<GatewaySettings, sqlx::Error> {
        let stored = sqlx::query_scalar::<_, String>(
            "SELECT value FROM settings WHERE key = 'default_timeout_seconds'",
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(GatewaySettings {
            default_timeout_seconds: stored
                .and_then(|value| value.parse().ok())
                .unwrap_or(DEFAULT_TIMEOUT_SECONDS),
        })
    }

    pub async fn update_settings(&self, settings: &GatewaySettings) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO settings(key, value)
             VALUES ('default_timeout_seconds', ?)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )
        .bind(settings.default_timeout_seconds.to_string())
        .execute(&self.pool)
        .await
        .map(|_| ())
    }
}
