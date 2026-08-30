use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use super::Database;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
pub enum ProviderKind {
    Searchix,
    TavilyHikari,
}

#[derive(Debug, Clone, FromRow)]
pub struct ProviderConfig {
    pub id: i64,
    pub kind: ProviderKind,
    pub name: String,
    pub endpoint: String,
    pub bearer_token: String,
    pub weight: i64,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct ProviderSummary {
    pub id: i64,
    pub kind: ProviderKind,
    pub name: String,
    pub endpoint: String,
    pub weight: i64,
    pub enabled: bool,
    pub token_configured: bool,
}

#[derive(Debug, Clone)]
pub struct NewProvider {
    pub kind: ProviderKind,
    pub name: String,
    pub endpoint: String,
    pub bearer_token: String,
    pub weight: i64,
    pub enabled: bool,
}

#[derive(Debug, Clone)]
pub struct ProviderUpdate {
    pub kind: ProviderKind,
    pub name: String,
    pub endpoint: String,
    pub bearer_token: Option<String>,
    pub weight: i64,
    pub enabled: bool,
}

impl Database {
    pub async fn providers(&self) -> Result<Vec<ProviderConfig>, sqlx::Error> {
        sqlx::query_as::<_, ProviderConfig>(
            "SELECT id, kind, name, endpoint, bearer_token, weight, enabled
             FROM providers
             ORDER BY id",
        )
        .fetch_all(&self.pool)
        .await
    }

    pub async fn provider(&self, id: i64) -> Result<Option<ProviderConfig>, sqlx::Error> {
        sqlx::query_as::<_, ProviderConfig>(
            "SELECT id, kind, name, endpoint, bearer_token, weight, enabled
             FROM providers
             WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
    }

    pub async fn provider_summaries(&self) -> Result<Vec<ProviderSummary>, sqlx::Error> {
        sqlx::query_as::<_, ProviderSummary>(
            "SELECT id, kind, name, endpoint, weight, enabled,
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
                (kind, name, endpoint, bearer_token, weight, enabled)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(provider.kind)
        .bind(provider.name)
        .bind(provider.endpoint)
        .bind(provider.bearer_token)
        .bind(provider.weight)
        .bind(provider.enabled)
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
                 updated_at = CURRENT_TIMESTAMP
             WHERE id = ?",
        )
        .bind(provider.kind)
        .bind(provider.name)
        .bind(provider.endpoint)
        .bind(provider.bearer_token)
        .bind(provider.weight)
        .bind(provider.enabled)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }
}
