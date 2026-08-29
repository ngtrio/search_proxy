use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{FromRow, SqlitePool, sqlite::SqlitePoolOptions};

#[derive(Clone)]
pub struct Database(pub SqlitePool);

#[derive(Clone, Serialize, FromRow)]
pub struct ProviderRow {
    pub id: i64,
    pub kind: String,
    pub name: String,
    pub endpoint: String,
    #[serde(skip_serializing)]
    pub bearer_token: String,
    pub weight: i64,
    pub enabled: bool,
    pub timeout_seconds: i64,
    pub token_configured: bool,
}

#[derive(Debug, Serialize, FromRow)]
pub struct ClientKeyRow {
    pub id: i64,
    pub name: String,
    pub prefix: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub request_count: i64,
}

impl Database {
    pub async fn connect(url: &str) -> anyhow::Result<Self> {
        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .after_connect(|conn, _| {
                Box::pin(async move {
                    sqlx::query("PRAGMA foreign_keys=ON")
                        .execute(&mut *conn)
                        .await?;
                    sqlx::query("PRAGMA journal_mode=WAL")
                        .execute(&mut *conn)
                        .await?;
                    sqlx::query("PRAGMA busy_timeout=5000")
                        .execute(&mut *conn)
                        .await?;
                    Ok(())
                })
            })
            .connect(url)
            .await?;
        sqlx::migrate!().run(&pool).await?;
        Ok(Self(pool))
    }

    pub async fn providers(&self) -> Result<Vec<ProviderRow>, sqlx::Error> {
        sqlx::query_as::<_, ProviderRow>(
            "SELECT id,kind,name,endpoint,bearer_token,weight,enabled,timeout_seconds,(bearer_token <> '') token_configured FROM providers ORDER BY id"
        ).fetch_all(&self.0).await
    }

    pub async fn provider(&self, id: i64) -> Result<Option<ProviderRow>, sqlx::Error> {
        sqlx::query_as::<_, ProviderRow>(
            "SELECT id,kind,name,endpoint,bearer_token,weight,enabled,timeout_seconds,(bearer_token <> '') token_configured FROM providers WHERE id=?"
        ).bind(id).fetch_optional(&self.0).await
    }
}
