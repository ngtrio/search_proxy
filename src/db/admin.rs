use chrono::{DateTime, Utc};
use sqlx::FromRow;

use super::Database;

#[derive(Debug, FromRow)]
pub struct AdminCredentials {
    pub username: String,
    pub password_hash: String,
}

#[derive(Debug, FromRow)]
pub struct AdminSession {
    pub id: String,
    pub csrf_digest: Vec<u8>,
}

impl Database {
    pub async fn admin_login_is_locked(&self, identity: &str) -> Result<bool, sqlx::Error> {
        let locked = sqlx::query_scalar::<_, bool>(
            "SELECT COALESCE(locked_until > CURRENT_TIMESTAMP, 0)
             FROM admin_login_throttle
             WHERE identity = ?",
        )
        .bind(identity)
        .fetch_optional(&self.pool)
        .await?;
        Ok(locked.unwrap_or(false))
    }

    pub async fn admin_credentials(&self) -> Result<Option<AdminCredentials>, sqlx::Error> {
        sqlx::query_as::<_, AdminCredentials>(
            "SELECT username, password_hash
             FROM admins
             WHERE id = 1",
        )
        .fetch_optional(&self.pool)
        .await
    }

    pub async fn record_admin_login_failure(&self, identity: &str) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO admin_login_throttle(identity, failed_attempts)
             VALUES (?, 1)
             ON CONFLICT(identity) DO UPDATE SET
                 failed_attempts = CASE
                     WHEN window_started_at < datetime('now', '-10 minutes') THEN 1
                     ELSE failed_attempts + 1
                 END,
                 window_started_at = CASE
                     WHEN window_started_at < datetime('now', '-10 minutes')
                         THEN CURRENT_TIMESTAMP
                     ELSE window_started_at
                 END,
                 locked_until = CASE
                     WHEN (
                         CASE
                             WHEN window_started_at < datetime('now', '-10 minutes') THEN 1
                             ELSE failed_attempts + 1
                         END
                     ) >= 5
                         THEN datetime('now', '+5 minutes')
                     ELSE locked_until
                 END",
        )
        .bind(identity)
        .execute(&self.pool)
        .await
        .map(|_| ())
    }

    pub async fn clear_admin_login_failures(&self, identity: &str) -> Result<(), sqlx::Error> {
        sqlx::query("DELETE FROM admin_login_throttle WHERE identity = ?")
            .bind(identity)
            .execute(&self.pool)
            .await
            .map(|_| ())
    }

    pub async fn create_admin_session(
        &self,
        id: &str,
        token_digest: &[u8],
        csrf_digest: &[u8],
        expires_at: DateTime<Utc>,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO admin_sessions(id, token_digest, csrf_digest, expires_at)
             VALUES (?, ?, ?, ?)",
        )
        .bind(id)
        .bind(token_digest.to_vec())
        .bind(csrf_digest.to_vec())
        .bind(expires_at)
        .execute(&self.pool)
        .await
        .map(|_| ())
    }

    pub async fn admin_session(
        &self,
        token_digest: &[u8],
    ) -> Result<Option<AdminSession>, sqlx::Error> {
        sqlx::query_as::<_, AdminSession>(
            "SELECT id, csrf_digest
             FROM admin_sessions
             WHERE token_digest = ? AND expires_at > CURRENT_TIMESTAMP",
        )
        .bind(token_digest.to_vec())
        .fetch_optional(&self.pool)
        .await
    }

    pub async fn delete_admin_session(&self, id: &str) -> Result<(), sqlx::Error> {
        sqlx::query("DELETE FROM admin_sessions WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await
            .map(|_| ())
    }

    pub async fn insert_admin_if_missing(
        &self,
        username: &str,
        password_hash: &str,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO admins(id, username, password_hash)
             VALUES (1, ?, ?)
             ON CONFLICT(id) DO NOTHING",
        )
        .bind(username)
        .bind(password_hash)
        .execute(&self.pool)
        .await
        .map(|_| ())
    }

    pub async fn admin_count(&self) -> Result<i64, sqlx::Error> {
        sqlx::query_scalar("SELECT COUNT(*) FROM admins")
            .fetch_one(&self.pool)
            .await
    }
}
