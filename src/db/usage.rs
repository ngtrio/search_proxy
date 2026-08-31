use serde::Serialize;
use sqlx::FromRow;

use super::Database;

#[derive(Debug, Clone, Copy)]
pub struct MetricsWindow {
    pub start: i64,
    pub end: i64,
    pub previous_start: i64,
    pub bucket_seconds: i64,
}

#[derive(Debug, Serialize, FromRow)]
pub struct MetricBucket {
    pub timestamp: String,
    pub requests: i64,
    pub failures: i64,
    pub p50_ms: Option<i64>,
    pub p95_ms: Option<i64>,
}

#[derive(Debug, Serialize, FromRow)]
pub struct ActivityBucket {
    pub timestamp: String,
    pub requests: i64,
}

#[derive(Debug, Serialize, FromRow)]
pub struct LatencyBucket {
    pub timestamp: String,
    pub p5_ms: Option<i64>,
    pub p50_ms: Option<i64>,
    pub p95_ms: Option<i64>,
}

#[derive(Debug, FromRow)]
pub struct MetricSummaryRow {
    pub current_requests: i64,
    pub current_successes: i64,
    pub current_p50: Option<i64>,
    pub current_p95: Option<i64>,
    pub previous_requests: i64,
    pub previous_successes: i64,
    pub previous_p50: Option<i64>,
    pub previous_p95: Option<i64>,
}

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
    pub async fn first_request_timestamp(&self) -> Result<Option<i64>, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT CAST(strftime('%s', MIN(started_at)) AS INTEGER) FROM request_events",
        )
        .fetch_one(&self.pool)
        .await
    }

    pub async fn metric_summary(
        &self,
        window: &MetricsWindow,
    ) -> Result<MetricSummaryRow, sqlx::Error> {
        sqlx::query_as::<_, MetricSummaryRow>(
            "WITH scoped AS (
                SELECT duration_ms, outcome,
                       CASE WHEN CAST(strftime('%s', started_at) AS INTEGER) >= ? THEN 1 ELSE 0 END AS current_period
                FROM request_events
                WHERE CAST(strftime('%s', started_at) AS INTEGER) >= ?
                  AND CAST(strftime('%s', started_at) AS INTEGER) < ?
             ), ranked AS (
                SELECT duration_ms, outcome, current_period,
                       ROW_NUMBER() OVER (PARTITION BY current_period ORDER BY duration_ms) AS rn,
                       COUNT(*) OVER (PARTITION BY current_period) AS cnt
                FROM scoped
             )
             SELECT
                COALESCE(SUM(current_period = 1), 0) AS current_requests,
                COALESCE(SUM(current_period = 1 AND outcome = 'success'), 0) AS current_successes,
                MAX(CASE WHEN current_period = 1 AND rn = (cnt + 1) / 2 THEN duration_ms END) AS current_p50,
                MAX(CASE WHEN current_period = 1 AND rn = (cnt * 95 + 99) / 100 THEN duration_ms END) AS current_p95,
                COALESCE(SUM(current_period = 0), 0) AS previous_requests,
                COALESCE(SUM(current_period = 0 AND outcome = 'success'), 0) AS previous_successes,
                MAX(CASE WHEN current_period = 0 AND rn = (cnt + 1) / 2 THEN duration_ms END) AS previous_p50,
                MAX(CASE WHEN current_period = 0 AND rn = (cnt * 95 + 99) / 100 THEN duration_ms END) AS previous_p95
             FROM ranked",
        )
        .bind(window.start)
        .bind(window.previous_start)
        .bind(window.end)
        .fetch_one(&self.pool)
        .await
    }

    pub async fn metric_series(
        &self,
        window: &MetricsWindow,
    ) -> Result<Vec<MetricBucket>, sqlx::Error> {
        sqlx::query_as::<_, MetricBucket>(
            "WITH RECURSIVE buckets(n, bucket_start) AS (
                SELECT 0, ?
                UNION ALL
                SELECT n + 1, bucket_start + ? FROM buckets
                WHERE bucket_start + ? < ?
             ), events AS (
                SELECT CAST((CAST(strftime('%s', started_at) AS INTEGER) - ?) / ? AS INTEGER) AS bucket,
                       duration_ms, outcome
                FROM request_events
                WHERE CAST(strftime('%s', started_at) AS INTEGER) >= ?
                  AND CAST(strftime('%s', started_at) AS INTEGER) < ?
             ), ranked AS (
                SELECT bucket, duration_ms, outcome,
                       ROW_NUMBER() OVER (PARTITION BY bucket ORDER BY duration_ms) AS rn,
                       COUNT(*) OVER (PARTITION BY bucket) AS cnt
                FROM events
             ), aggregate AS (
                SELECT bucket, COUNT(*) AS requests,
                       SUM(outcome <> 'success') AS failures,
                       MAX(CASE WHEN rn = (cnt + 1) / 2 THEN duration_ms END) AS p50_ms,
                       MAX(CASE WHEN rn = (cnt * 95 + 99) / 100 THEN duration_ms END) AS p95_ms
                FROM ranked GROUP BY bucket
             )
             SELECT strftime('%Y-%m-%dT%H:%M:%SZ', b.bucket_start, 'unixepoch') AS timestamp,
                    COALESCE(a.requests, 0) AS requests,
                    COALESCE(a.failures, 0) AS failures,
                    a.p50_ms, a.p95_ms
             FROM buckets b LEFT JOIN aggregate a ON a.bucket = b.n ORDER BY b.n",
        )
        .bind(window.start)
        .bind(window.bucket_seconds)
        .bind(window.bucket_seconds)
        .bind(window.end)
        .bind(window.start)
        .bind(window.bucket_seconds)
        .bind(window.start)
        .bind(window.end)
        .fetch_all(&self.pool)
        .await
    }

    pub async fn activity_series(
        &self,
        window: &MetricsWindow,
    ) -> Result<Vec<ActivityBucket>, sqlx::Error> {
        sqlx::query_as::<_, ActivityBucket>(
            "WITH RECURSIVE buckets(n, bucket_start) AS (
                SELECT 0, ? UNION ALL SELECT n + 1, bucket_start + ? FROM buckets
                WHERE bucket_start + ? < ?
             ), aggregate AS (
                SELECT CAST((CAST(strftime('%s', started_at) AS INTEGER) - ?) / ? AS INTEGER) AS bucket,
                       COUNT(*) AS requests
                FROM request_events
                WHERE CAST(strftime('%s', started_at) AS INTEGER) >= ?
                  AND CAST(strftime('%s', started_at) AS INTEGER) < ? GROUP BY bucket
             )
             SELECT strftime('%Y-%m-%dT%H:%M:%SZ', b.bucket_start, 'unixepoch') AS timestamp,
                    COALESCE(a.requests, 0) AS requests
             FROM buckets b LEFT JOIN aggregate a ON a.bucket = b.n ORDER BY b.n",
        )
        .bind(window.start).bind(window.bucket_seconds).bind(window.bucket_seconds).bind(window.end)
        .bind(window.start).bind(window.bucket_seconds).bind(window.start).bind(window.end)
        .fetch_all(&self.pool).await
    }

    pub async fn latency_distribution(
        &self,
        start: i64,
        end: i64,
    ) -> Result<Vec<LatencyBucket>, sqlx::Error> {
        sqlx::query_as::<_, LatencyBucket>(
            "WITH RECURSIVE buckets(n, bucket_start) AS (
                SELECT 0, ? UNION ALL SELECT n + 1, bucket_start + 3600 FROM buckets
                WHERE bucket_start + 3600 < ?
             ), events AS (
                SELECT CAST((CAST(strftime('%s', started_at) AS INTEGER) - ?) / 3600 AS INTEGER) AS bucket,
                       duration_ms FROM request_events
                WHERE CAST(strftime('%s', started_at) AS INTEGER) >= ?
                  AND CAST(strftime('%s', started_at) AS INTEGER) < ?
             ), ranked AS (
                SELECT bucket, duration_ms,
                       ROW_NUMBER() OVER (PARTITION BY bucket ORDER BY duration_ms) AS rn,
                       COUNT(*) OVER (PARTITION BY bucket) AS cnt FROM events
             ), aggregate AS (
                SELECT bucket,
                       MAX(CASE WHEN rn = (cnt * 5 + 99) / 100 THEN duration_ms END) AS p5_ms,
                       MAX(CASE WHEN rn = (cnt + 1) / 2 THEN duration_ms END) AS p50_ms,
                       MAX(CASE WHEN rn = (cnt * 95 + 99) / 100 THEN duration_ms END) AS p95_ms
                FROM ranked GROUP BY bucket
             )
             SELECT strftime('%Y-%m-%dT%H:%M:%SZ', b.bucket_start, 'unixepoch') AS timestamp,
                    a.p5_ms, a.p50_ms, a.p95_ms
             FROM buckets b LEFT JOIN aggregate a ON a.bucket = b.n ORDER BY b.n",
        )
        .bind(start).bind(end).bind(start).bind(start).bind(end)
        .fetch_all(&self.pool).await
    }

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
                    CAST(COALESCE(AVG(duration_ms), 0) AS INTEGER) AS average_latency_ms
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
                    CAST(COALESCE(AVG(e.duration_ms), 0) AS INTEGER) AS average_latency_ms
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
                    CAST(CASE WHEN SUM(requests) > 0
                              THEN SUM(duration_ms) / SUM(requests)
                              ELSE 0
                         END AS INTEGER) AS average_latency_ms
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
