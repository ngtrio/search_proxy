use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::FromRow;

use super::Database;

#[derive(Debug, Serialize, FromRow)]
pub struct ClientKeySummary {
    pub id: i64,
    pub name: String,
    pub prefix: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub request_count: i64,
    pub key: Option<String>,
}

impl Database {
    pub async fn active_client_key_id(
        &self,
        token_digest: &[u8],
    ) -> Result<Option<i64>, sqlx::Error> {
        sqlx::query_scalar::<_, i64>(
            "SELECT id
             FROM client_api_keys
             WHERE status = 'active' AND digest = ?",
        )
        .bind(token_digest.to_vec())
        .fetch_optional(&self.pool)
        .await
    }

    pub async fn client_key_summaries(&self) -> Result<Vec<ClientKeySummary>, sqlx::Error> {
        sqlx::query_as::<_, ClientKeySummary>(
            "SELECT id, name, prefix, status, created_at, last_used_at, request_count,
                    CASE WHEN status = 'active' THEN secret ELSE NULL END AS key
             FROM client_api_keys
             ORDER BY id DESC",
        )
        .fetch_all(&self.pool)
        .await
    }

    pub async fn create_client_key(
        &self,
        name: &str,
        prefix: &str,
        token_digest: &[u8],
        secret: &str,
    ) -> Result<i64, sqlx::Error> {
        let result = sqlx::query(
            "INSERT INTO client_api_keys(name, prefix, digest, secret)
             VALUES (?, ?, ?, ?)",
        )
        .bind(name)
        .bind(prefix)
        .bind(token_digest.to_vec())
        .bind(secret)
        .execute(&self.pool)
        .await?;
        Ok(result.last_insert_rowid())
    }

    pub async fn mark_client_key_used(&self, id: i64) -> Result<(), sqlx::Error> {
        sqlx::query(
            "UPDATE client_api_keys
             SET last_used_at = CURRENT_TIMESTAMP, request_count = request_count + 1
             WHERE id = ?",
        )
        .bind(id)
        .execute(&self.pool)
        .await
        .map(|_| ())
    }
}
