use serde::Serialize;
use sqlx::FromRow;

use super::Database;

#[derive(Debug, Clone, FromRow)]
pub struct ProviderConfig {
    pub id: i64,
    pub kind: String,
    pub name: String,
    pub endpoint: String,
    pub bearer_token: String,
    pub weight: i64,
    pub enabled: bool,
    pub timeout_seconds: i64,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct ProviderSummary {
    pub id: i64,
    pub kind: String,
    pub name: String,
    pub endpoint: String,
    pub weight: i64,
    pub enabled: bool,
    pub timeout_seconds: i64,
    pub token_configured: bool,
}

#[derive(Debug, Clone)]
pub struct NewProvider {
    pub kind: String,
    pub name: String,
    pub endpoint: String,
    pub bearer_token: String,
    pub weight: i64,
    pub enabled: bool,
    pub timeout_seconds: i64,
}

#[derive(Debug, Clone)]
pub struct ProviderUpdate {
    pub kind: String,
    pub name: String,
    pub endpoint: String,
    pub bearer_token: Option<String>,
    pub weight: i64,
    pub enabled: bool,
    pub timeout_seconds: i64,
}

impl Database {
    pub async fn providers(&self) -> Result<Vec<ProviderConfig>, sqlx::Error> {
        sqlx::query_as::<_, ProviderConfig>(
            "SELECT id, kind, name, endpoint, bearer_token, weight, enabled, timeout_seconds
             FROM providers
             ORDER BY id",
        )
        .fetch_all(&self.pool)
        .await
    }

    pub async fn provider(&self, id: i64) -> Result<Option<ProviderConfig>, sqlx::Error> {
        sqlx::query_as::<_, ProviderConfig>(
            "SELECT id, kind, name, endpoint, bearer_token, weight, enabled, timeout_seconds
             FROM providers
             WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
    }

    pub async fn provider_summaries(&self) -> Result<Vec<ProviderSummary>, sqlx::Error> {
        sqlx::query_as::<_, ProviderSummary>(
            "SELECT id, kind, name, endpoint, weight, enabled, timeout_seconds,
                    (bearer_token <> '') AS token_configured
             FROM providers
             ORDER BY id",
        )
        .fetch_all(&self.pool)
        .await
    }

    pub async fn create_provider(&self, provider: NewProvider) -> Result<i64, sqlx::Error> {
        let result = sqlx::query(
            "INSERT INTO providers
                (kind, name, endpoint, bearer_token, weight, enabled, timeout_seconds)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(provider.kind)
        .bind(provider.name)
        .bind(provider.endpoint)
        .bind(provider.bearer_token)
        .bind(provider.weight)
        .bind(provider.enabled)
        .bind(provider.timeout_seconds)
        .execute(&self.pool)
        .await?;
        Ok(result.last_insert_rowid())
    }

    pub async fn update_provider(
        &self,
        id: i64,
        provider: ProviderUpdate,
    ) -> Result<bool, sqlx::Error> {
        let result = sqlx::query(
            "UPDATE providers
             SET kind = ?,
                 name = ?,
                 endpoint = ?,
                 bearer_token = COALESCE(NULLIF(?, ''), bearer_token),
                 weight = ?,
                 enabled = ?,
                 timeout_seconds = ?,
                 updated_at = CURRENT_TIMESTAMP
             WHERE id = ?",
        )
        .bind(provider.kind)
        .bind(provider.name)
        .bind(provider.endpoint)
        .bind(provider.bearer_token)
        .bind(provider.weight)
        .bind(provider.enabled)
        .bind(provider.timeout_seconds)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }
}
