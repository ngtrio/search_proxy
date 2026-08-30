use serde::Serialize;
use sqlx::FromRow;

use super::Database;

#[derive(Debug, Clone, Copy)]
pub struct RequestRecord<'a> {
    pub id: &'a str,
    pub client_key_id: i64,
    pub provider_id: Option<i64>,
    pub duration_ms: i64,
    pub outcome: &'a str,
    pub error_category: Option<&'a str>,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct ProviderMetric {
    pub provider: String,
    pub requests: i64,
    pub successes: i64,
    pub failures: i64,
    pub average_latency_ms: i64,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct DailyMetric {
    pub day: String,
    pub requests: i64,
    pub successes: i64,
    pub failures: i64,
    pub average_latency_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageOverview {
    pub requests: i64,
    pub successes: i64,
    pub failures: i64,
    pub average_latency_ms: i64,
    pub providers: Vec<ProviderMetric>,
    pub daily: Vec<DailyMetric>,
}

#[derive(Debug, Serialize, FromRow)]
pub struct RequestSummary {
    pub request_id: String,
    pub client_key_prefix: Option<String>,
    pub client_key_name: Option<String>,
    pub provider: Option<String>,
    pub started_at: String,
    pub duration_ms: i64,
    pub outcome: String,
    pub error_category: Option<String>,
}

#[derive(Debug, FromRow)]
struct OverallMetric {
    requests: i64,
    successes: i64,
    average_latency_ms: i64,
}

impl Database {
    pub async fn record_request(&self, record: RequestRecord<'_>) -> Result<(), sqlx::Error> {
        let mut transaction = self.pool.begin().await?;

        sqlx::query(
            "INSERT INTO request_events
                (id, client_key_id, provider_id, duration_ms, outcome, error_category)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(record.id)
        .bind(record.client_key_id)
        .bind(record.provider_id)
        .bind(record.duration_ms)
        .bind(record.outcome)
        .bind(record.error_category)
        .execute(&mut *transaction)
        .await?;

        let updated = sqlx::query(
            "UPDATE usage_daily
             SET requests = requests + 1, duration_ms = duration_ms + ?
             WHERE day = date('now')
               AND client_key_id = ?
               AND provider_id IS ?
               AND outcome = ?",
        )
        .bind(record.duration_ms)
        .bind(record.client_key_id)
        .bind(record.provider_id)
        .bind(record.outcome)
        .execute(&mut *transaction)
        .await?;

        if updated.rows_affected() == 0 {
            sqlx::query(
                "INSERT INTO usage_daily
                    (day, client_key_id, provider_id, outcome, requests, duration_ms)
                 VALUES (date('now'), ?, ?, ?, 1, ?)",
            )
            .bind(record.client_key_id)
            .bind(record.provider_id)
            .bind(record.outcome)
            .bind(record.duration_ms)
            .execute(&mut *transaction)
            .await?;
        }

        transaction.commit().await
    }

    pub async fn usage_overview(&self) -> Result<UsageOverview, sqlx::Error> {
        let overall = sqlx::query_as::<_, OverallMetric>(
            "SELECT COUNT(*) AS requests,
                    COALESCE(SUM(outcome = 'success'), 0) AS successes,
                    COALESCE(AVG(duration_ms), 0) AS average_latency_ms
             FROM request_events
             WHERE started_at >= datetime('now', '-30 days')",
        )
        .fetch_one(&self.pool)
        .await?;

        let providers = sqlx::query_as::<_, ProviderMetric>(
            "SELECT p.name AS provider,
                    COUNT(*) AS requests,
                    COALESCE(SUM(e.outcome = 'success'), 0) AS successes,
                    COALESCE(SUM(e.outcome <> 'success'), 0) AS failures,
                    COALESCE(AVG(e.duration_ms), 0) AS average_latency_ms
             FROM request_events e
             JOIN providers p ON p.id = e.provider_id
             WHERE e.started_at >= datetime('now', '-30 days')
             GROUP BY p.id, p.name
             ORDER BY p.name",
        )
        .fetch_all(&self.pool)
        .await?;

        let daily = sqlx::query_as::<_, DailyMetric>(
            "SELECT day,
                    SUM(requests) AS requests,
                    COALESCE(SUM(CASE WHEN outcome = 'success' THEN requests ELSE 0 END), 0)
                        AS successes,
                    COALESCE(SUM(CASE WHEN outcome <> 'success' THEN requests ELSE 0 END), 0)
                        AS failures,
                    CASE WHEN SUM(requests) > 0
                         THEN SUM(duration_ms) / SUM(requests)
                         ELSE 0
                    END AS average_latency_ms
             FROM usage_daily
             WHERE day >= date('now', '-30 days')
             GROUP BY day
             ORDER BY day",
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(UsageOverview {
            requests: overall.requests,
            successes: overall.successes,
            failures: overall.requests - overall.successes,
            average_latency_ms: overall.average_latency_ms,
            providers,
            daily,
        })
    }

    pub async fn recent_requests(&self) -> Result<Vec<RequestSummary>, sqlx::Error> {
        sqlx::query_as::<_, RequestSummary>(
            "SELECT e.id AS request_id,
                    k.prefix AS client_key_prefix,
                    k.name AS client_key_name,
                    p.name AS provider,
                    e.started_at,
                    e.duration_ms,
                    e.outcome,
                    e.error_category
             FROM request_events e
             LEFT JOIN client_api_keys k ON k.id = e.client_key_id
             LEFT JOIN providers p ON p.id = e.provider_id
             ORDER BY e.started_at DESC
             LIMIT 200",
        )
        .fetch_all(&self.pool)
        .await
    }
}
