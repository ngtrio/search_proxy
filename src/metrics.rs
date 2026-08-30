use axum::{
    Json,
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{AppState, db::MetricsWindow};

#[derive(Debug, Deserialize)]
pub struct MetricsQuery {
    window: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Window {
    OneHour,
    TwentyFourHours,
    SevenDays,
    ThirtyDays,
}

impl Window {
    fn parse(value: Option<&str>) -> Option<Self> {
        match value.unwrap_or("24h") {
            "1h" => Some(Self::OneHour),
            "24h" => Some(Self::TwentyFourHours),
            "7d" => Some(Self::SevenDays),
            "30d" => Some(Self::ThirtyDays),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::OneHour => "1h",
            Self::TwentyFourHours => "24h",
            Self::SevenDays => "7d",
            Self::ThirtyDays => "30d",
        }
    }

    fn duration(self) -> Duration {
        match self {
            Self::OneHour => Duration::hours(1),
            Self::TwentyFourHours => Duration::hours(24),
            Self::SevenDays => Duration::days(7),
            Self::ThirtyDays => Duration::days(30),
        }
    }

    fn bucket_seconds(self) -> i64 {
        match self {
            Self::OneHour => 60,
            Self::TwentyFourHours => 300,
            Self::SevenDays => 3_600,
            Self::ThirtyDays => 21_600,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct MetricValue<T> {
    pub value: T,
    pub change: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct Summary {
    pub requests: MetricValue<i64>,
    pub success_rate: MetricValue<Option<f64>>,
    pub p50_ms: MetricValue<Option<i64>>,
    pub p95_ms: MetricValue<Option<i64>>,
}

#[derive(Debug, Serialize)]
pub struct MetricsResponse {
    pub window: &'static str,
    pub generated_at: String,
    pub timezone: &'static str,
    pub summary: Summary,
    pub series: Vec<crate::db::MetricBucket>,
    pub activity: Vec<crate::db::ActivityBucket>,
    pub latency_distribution: Vec<crate::db::LatencyBucket>,
}

fn iso(time: DateTime<Utc>) -> String {
    time.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn percent_change(current: i64, previous: i64) -> Option<f64> {
    (previous > 0).then(|| (current - previous) as f64 / previous as f64 * 100.0)
}

fn difference(current: Option<i64>, previous: Option<i64>) -> Option<f64> {
    Some((current? - previous?) as f64)
}

pub async fn handler(State(state): State<AppState>, Query(query): Query<MetricsQuery>) -> Response {
    let Some(window) = Window::parse(query.window.as_deref()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"window must be one of 1h, 24h, 7d, 30d"})),
        )
            .into_response();
    };

    let generated_at = Utc::now();
    // SQLite timestamps have whole-second precision. Make the exclusive upper
    // bound the next second so requests written during this second are visible.
    let end = generated_at + Duration::seconds(1);
    let start = end - window.duration();
    let previous_start = start - window.duration();
    let metrics_window = MetricsWindow {
        start: start.timestamp(),
        end: end.timestamp(),
        previous_start: previous_start.timestamp(),
        bucket_seconds: window.bucket_seconds(),
    };

    let result = async {
        let summary = state.db.metric_summary(&metrics_window).await?;
        let series = state.db.metric_series(&metrics_window).await?;
        let fixed_24h = MetricsWindow {
            start: (end - Duration::hours(24)).timestamp(),
            end: end.timestamp(),
            previous_start: 0,
            bucket_seconds: 300,
        };
        let activity = state.db.activity_series(&fixed_24h).await?;
        let latency_distribution = state
            .db
            .latency_distribution(fixed_24h.start, fixed_24h.end)
            .await?;
        Ok::<_, sqlx::Error>((summary, series, activity, latency_distribution))
    }
    .await;

    match result {
        Ok((values, series, activity, latency_distribution)) => {
            let current_rate = (values.current_requests > 0)
                .then(|| values.current_successes as f64 / values.current_requests as f64 * 100.0);
            let previous_rate = (values.previous_requests > 0).then(|| {
                values.previous_successes as f64 / values.previous_requests as f64 * 100.0
            });
            Json(MetricsResponse {
                window: window.label(),
                generated_at: iso(generated_at),
                timezone: "UTC",
                summary: Summary {
                    requests: MetricValue {
                        value: values.current_requests,
                        change: percent_change(values.current_requests, values.previous_requests),
                    },
                    success_rate: MetricValue {
                        value: current_rate,
                        change: current_rate.zip(previous_rate).map(|(a, b)| a - b),
                    },
                    p50_ms: MetricValue {
                        value: values.current_p50,
                        change: difference(values.current_p50, values.previous_p50),
                    },
                    p95_ms: MetricValue {
                        value: values.current_p95,
                        change: difference(values.current_p95, values.previous_p95),
                    },
                },
                series,
                activity,
                latency_distribution,
            })
            .into_response()
        }
        Err(error) => {
            tracing::error!(%error, "could not load public metrics");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error":"could not load metrics"})),
            )
                .into_response()
        }
    }
}
