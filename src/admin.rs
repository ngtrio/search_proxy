use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
    routing::{get, post, put},
};
use chrono::{Duration, Utc};
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::FromRow;

use crate::{
    AppState,
    auth::{digest, hash_password, random_secret, verify_digest, verify_password},
    db::ClientKeyRow,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/session", get(session))
        .route("/login", post(login))
        .route("/logout", post(logout))
        .route("/keys", get(keys).post(create_key))
        .route("/keys/{id}/revoke", post(revoke_key))
        .route("/providers", get(providers).post(create_provider))
        .route("/providers/{id}", put(update_provider))
        .route("/overview", get(overview))
        .route("/requests", get(requests))
        .route("/settings", get(settings).put(update_settings))
}

pub async fn ready(State(state): State<AppState>) -> impl IntoResponse {
    let db_ok = sqlx::query_scalar::<_, i64>("SELECT 1")
        .fetch_one(&state.db.0)
        .await
        .is_ok();
    let ready = db_ok && state.providers.has_providers();
    (
        if ready {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        Json(json!({"ready":ready,"database":db_ok})),
    )
}

#[derive(RustEmbed)]
#[folder = "web/dist/"]
struct AdminAssets;

pub async fn spa() -> impl IntoResponse {
    asset_response("index.html")
}

pub async fn asset(Path(path): Path<String>) -> impl IntoResponse {
    asset_response(&format!("assets/{path}"))
}

fn asset_response(path: &str) -> axum::response::Response {
    match AdminAssets::get(path) {
        Some(file) => (
            StatusCode::OK,
            [(
                header::CONTENT_TYPE,
                mime_guess::from_path(path).first_or_octet_stream().as_ref(),
            )],
            file.data,
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

#[derive(Deserialize)]
struct Login {
    username: String,
    password: String,
}
async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<Login>,
) -> impl IntoResponse {
    if !same_origin(&state, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let locked: bool = sqlx::query_scalar(
        "SELECT locked_until > CURRENT_TIMESTAMP FROM admin_login_throttle WHERE identity=?",
    )
    .bind(&input.username)
    .fetch_optional(&state.db.0)
    .await
    .ok()
    .flatten()
    .unwrap_or(false);
    if locked {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({"error":"authentication temporarily unavailable"})),
        )
            .into_response();
    }
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT username,password_hash FROM admins WHERE id=1")
            .fetch_optional(&state.db.0)
            .await
            .ok()
            .flatten();
    if !row.is_some_and(|(username, hash)| {
        username == input.username && verify_password(&input.password, &hash)
    }) {
        let _ = sqlx::query("INSERT INTO admin_login_throttle(identity,failed_attempts) VALUES(?,1) ON CONFLICT(identity) DO UPDATE SET failed_attempts=CASE WHEN window_started_at < datetime('now','-10 minutes') THEN 1 ELSE failed_attempts+1 END,window_started_at=CASE WHEN window_started_at < datetime('now','-10 minutes') THEN CURRENT_TIMESTAMP ELSE window_started_at END,locked_until=CASE WHEN (CASE WHEN window_started_at < datetime('now','-10 minutes') THEN 1 ELSE failed_attempts+1 END)>=5 THEN datetime('now','+5 minutes') ELSE locked_until END")
            .bind(&input.username).execute(&state.db.0).await;
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"invalid credentials"})),
        )
            .into_response();
    }
    let _ = sqlx::query("DELETE FROM admin_login_throttle WHERE identity=?")
        .bind(&input.username)
        .execute(&state.db.0)
        .await;
    let token = random_secret(32);
    let csrf = random_secret(32);
    let id = random_secret(16);
    let expires = Utc::now() + Duration::hours(8);
    if sqlx::query(
        "INSERT INTO admin_sessions(id,token_digest,csrf_digest,expires_at) VALUES(?,?,?,?)",
    )
    .bind(id)
    .bind(digest(&token))
    .bind(digest(&csrf))
    .bind(expires)
    .execute(&state.db.0)
    .await
    .is_err()
    {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    let secure = if state.config.production {
        "; Secure"
    } else {
        ""
    };
    let cookie = format!(
        "gateway_session={token}; Path=/admin; HttpOnly; SameSite=Strict; Max-Age=28800{secure}"
    );
    let csrf_cookie =
        format!("gateway_csrf={csrf}; Path=/admin; SameSite=Strict; Max-Age=28800{secure}");
    let mut response = (
        StatusCode::OK,
        Json(json!({"csrf_token":csrf,"expires_at":expires})),
    )
        .into_response();
    response.headers_mut().append(
        header::SET_COOKIE,
        cookie.parse().expect("valid session cookie"),
    );
    response.headers_mut().append(
        header::SET_COOKIE,
        csrf_cookie.parse().expect("valid csrf cookie"),
    );
    response
}

async fn session(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    match admin_auth(&state, &headers, false).await {
        Some(_) => (StatusCode::OK, Json(json!({"authenticated":true}))),
        None => (
            StatusCode::UNAUTHORIZED,
            Json(json!({"authenticated":false})),
        ),
    }
}
async fn logout(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let Some(id) = admin_auth(&state, &headers, true).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let _ = sqlx::query("DELETE FROM admin_sessions WHERE id=?")
        .bind(id)
        .execute(&state.db.0)
        .await;
    let mut response = (StatusCode::NO_CONTENT,).into_response();
    response.headers_mut().append(
        header::SET_COOKIE,
        "gateway_session=; Path=/admin; HttpOnly; SameSite=Strict; Max-Age=0"
            .parse()
            .expect("valid cookie"),
    );
    response.headers_mut().append(
        header::SET_COOKIE,
        "gateway_csrf=; Path=/admin; SameSite=Strict; Max-Age=0"
            .parse()
            .expect("valid cookie"),
    );
    response
}

async fn admin_auth(state: &AppState, headers: &HeaderMap, write: bool) -> Option<String> {
    if write && !same_origin(state, headers) {
        return None;
    }
    let cookies = headers.get(header::COOKIE)?.to_str().ok()?;
    let token = cookies
        .split(';')
        .map(str::trim)
        .find_map(|p| p.strip_prefix("gateway_session="))?;
    let rows: Vec<(String,Vec<u8>,Vec<u8>)> = sqlx::query_as("SELECT id,token_digest,csrf_digest FROM admin_sessions WHERE expires_at > CURRENT_TIMESTAMP").fetch_all(&state.db.0).await.ok()?;
    let (id, _, csrf) = rows
        .into_iter()
        .find(|(_, expected, _)| verify_digest(token, expected))?;
    if write {
        let supplied = headers.get("x-csrf-token")?.to_str().ok()?;
        if !verify_digest(supplied, &csrf) {
            return None;
        }
    }
    Some(id)
}

fn same_origin(state: &AppState, headers: &HeaderMap) -> bool {
    let Some(expected) = &state.config.public_origin else {
        return !state.config.production;
    };
    headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) == Some(expected.as_str())
}

async fn keys(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if admin_auth(&state, &headers, false).await.is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let rows = sqlx::query_as::<_,ClientKeyRow>("SELECT id,name,prefix,status,created_at,last_used_at,request_count FROM client_api_keys ORDER BY id DESC").fetch_all(&state.db.0).await.unwrap_or_default();
    Json(rows).into_response()
}
#[derive(Deserialize)]
struct Name {
    name: String,
}
async fn create_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<Name>,
) -> impl IntoResponse {
    if admin_auth(&state, &headers, true).await.is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if input.name.trim().is_empty() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let secret = format!("tmg_{}", random_secret(32));
    let prefix = secret.chars().take(12).collect::<String>();
    match sqlx::query("INSERT INTO client_api_keys(name,prefix,digest) VALUES(?,?,?)")
        .bind(input.name)
        .bind(&prefix)
        .bind(digest(&secret))
        .execute(&state.db.0)
        .await
    {
        Ok(result) => (
            StatusCode::CREATED,
            Json(json!({"id":result.last_insert_rowid(),"prefix":prefix,"key":secret})),
        )
            .into_response(),
        Err(_) => StatusCode::BAD_REQUEST.into_response(),
    }
}
async fn revoke_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    if admin_auth(&state, &headers, true).await.is_none() {
        return StatusCode::UNAUTHORIZED;
    }
    let _ = sqlx::query("UPDATE client_api_keys SET status='revoked' WHERE id=?")
        .bind(id)
        .execute(&state.db.0)
        .await;
    StatusCode::NO_CONTENT
}

async fn providers(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if admin_auth(&state, &headers, false).await.is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match state.db.providers().await {
        Ok(rows) => Json(rows).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
#[derive(Deserialize)]
struct ProviderInput {
    kind: String,
    name: String,
    endpoint: String,
    token: Option<String>,
    weight: i64,
    enabled: bool,
    timeout_seconds: i64,
}
async fn create_provider(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<ProviderInput>,
) -> impl IntoResponse {
    if admin_auth(&state, &headers, true).await.is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if !valid_provider(&input) || input.token.as_deref().is_none_or(str::is_empty) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let result = sqlx::query("INSERT INTO providers(kind,name,endpoint,bearer_token,weight,enabled,timeout_seconds) VALUES(?,?,?,?,?,?,?)")
        .bind(input.kind).bind(input.name).bind(input.endpoint).bind(input.token.unwrap()).bind(input.weight).bind(input.enabled).bind(input.timeout_seconds).execute(&state.db.0).await;
    match result {
        Ok(r) => {
            let id = r.last_insert_rowid();
            let provider = match state.db.provider(id).await {
                Ok(Some(provider)) => provider,
                _ => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
            };
            if state.providers.add(provider).await.is_err() {
                return StatusCode::BAD_GATEWAY.into_response();
            }
            (StatusCode::CREATED, Json(json!({"id":id}))).into_response()
        }
        Err(_) => StatusCode::BAD_REQUEST.into_response(),
    }
}
async fn update_provider(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(input): Json<ProviderInput>,
) -> impl IntoResponse {
    if admin_auth(&state, &headers, true).await.is_none() {
        return StatusCode::UNAUTHORIZED;
    }
    if !valid_provider(&input) {
        return StatusCode::BAD_REQUEST;
    }
    let updated = sqlx::query("UPDATE providers SET kind=?,name=?,endpoint=?,bearer_token=COALESCE(NULLIF(?,''),bearer_token),weight=?,enabled=?,timeout_seconds=?,updated_at=CURRENT_TIMESTAMP WHERE id=?")
        .bind(input.kind).bind(input.name).bind(input.endpoint).bind(input.token).bind(input.weight).bind(input.enabled).bind(input.timeout_seconds).bind(id).execute(&state.db.0).await;
    if updated.is_err() || updated.is_ok_and(|result| result.rows_affected() == 0) {
        return StatusCode::NOT_FOUND;
    }
    let provider = match state.db.provider(id).await {
        Ok(Some(provider)) => provider,
        _ => return StatusCode::INTERNAL_SERVER_ERROR,
    };
    if state.providers.update(provider).await.is_err() {
        return StatusCode::BAD_GATEWAY;
    }
    StatusCode::NO_CONTENT
}
fn valid_provider(p: &ProviderInput) -> bool {
    ["searchix", "tavily_hikari"].contains(&p.kind.as_str())
        && !p.name.trim().is_empty()
        && p.endpoint.parse::<url::Url>().is_ok_and(|u| {
            u.username().is_empty()
                && u.password().is_none()
                && (u.scheme() == "https" || u.host_str() == Some("127.0.0.1"))
        })
        && p.weight > 0
        && p.timeout_seconds > 0
}
async fn overview(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if admin_auth(&state, &headers, false).await.is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    #[derive(FromRow, Serialize)]
    struct ProviderMetric {
        provider: String,
        requests: i64,
        successes: i64,
        failures: i64,
        average_latency_ms: i64,
    }
    #[derive(FromRow, Serialize)]
    struct DailyMetric {
        day: String,
        requests: i64,
        successes: i64,
        failures: i64,
        average_latency_ms: i64,
    }
    let row: (i64,i64,i64)=sqlx::query_as("SELECT COUNT(*),COALESCE(SUM(outcome='success'),0),COALESCE(AVG(duration_ms),0) FROM request_events WHERE started_at >= datetime('now','-30 days')").fetch_one(&state.db.0).await.unwrap_or((0,0,0));
    let providers: Vec<ProviderMetric> = sqlx::query_as("SELECT p.name provider,COUNT(*) requests,COALESCE(SUM(e.outcome='success'),0) successes,COALESCE(SUM(e.outcome<>'success'),0) failures,COALESCE(AVG(e.duration_ms),0) average_latency_ms FROM request_events e JOIN providers p ON p.id=e.provider_id WHERE e.started_at >= datetime('now','-30 days') GROUP BY p.id,p.name ORDER BY p.name")
        .fetch_all(&state.db.0).await.unwrap_or_default();
    let daily: Vec<DailyMetric> = sqlx::query_as("SELECT day,SUM(requests) requests,COALESCE(SUM(CASE WHEN outcome='success' THEN requests ELSE 0 END),0) successes,COALESCE(SUM(CASE WHEN outcome<>'success' THEN requests ELSE 0 END),0) failures,CASE WHEN SUM(requests)>0 THEN SUM(duration_ms)/SUM(requests) ELSE 0 END average_latency_ms FROM usage_daily WHERE day >= date('now','-30 days') GROUP BY day ORDER BY day")
        .fetch_all(&state.db.0).await.unwrap_or_default();
    Json(json!({"requests":row.0,"successes":row.1,"failures":row.0-row.1,"average_latency_ms":row.2,"providers":providers,"daily":daily})).into_response()
}
async fn requests(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if admin_auth(&state, &headers, false).await.is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    #[derive(FromRow)]
    struct RequestRow {
        id: String,
        prefix: Option<String>,
        client_key_name: Option<String>,
        provider: Option<String>,
        started_at: String,
        duration_ms: i64,
        outcome: String,
        error_category: Option<String>,
    }
    let rows: Vec<RequestRow> = sqlx::query_as("SELECT e.id,k.prefix,k.name client_key_name,p.name provider,e.started_at,e.duration_ms,e.outcome,e.error_category FROM request_events e LEFT JOIN client_api_keys k ON k.id=e.client_key_id LEFT JOIN providers p ON p.id=e.provider_id ORDER BY e.started_at DESC LIMIT 200").fetch_all(&state.db.0).await.unwrap_or_default();
    Json(rows.into_iter().map(|r|json!({"request_id":r.id,"client_key_prefix":r.prefix,"client_key_name":r.client_key_name,"provider":r.provider,"started_at":r.started_at,"duration_ms":r.duration_ms,"outcome":r.outcome,"error_category":r.error_category})).collect::<Vec<Value>>()).into_response()
}
async fn settings(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if admin_auth(&state, &headers, false).await.is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let rows: Vec<(String, String)> = sqlx::query_as("SELECT key,value FROM settings")
        .fetch_all(&state.db.0)
        .await
        .unwrap_or_default();
    let values = rows
        .into_iter()
        .collect::<std::collections::HashMap<_, _>>();
    Json(json!({
        "default_timeout_seconds":values.get("default_timeout_seconds").and_then(|v|v.parse::<i64>().ok()).unwrap_or(120)
    })).into_response()
}

#[derive(Deserialize)]
struct SettingsInput {
    default_timeout_seconds: i64,
}
async fn update_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<SettingsInput>,
) -> impl IntoResponse {
    if admin_auth(&state, &headers, true).await.is_none() {
        return StatusCode::UNAUTHORIZED;
    }
    if input.default_timeout_seconds <= 0 {
        return StatusCode::BAD_REQUEST;
    }
    let mut tx = match state.db.0.begin().await {
        Ok(tx) => tx,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR,
    };
    for (key, value) in [("default_timeout_seconds", input.default_timeout_seconds)] {
        if sqlx::query("INSERT INTO settings(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value").bind(key).bind(value.to_string()).execute(&mut *tx).await.is_err() { return StatusCode::INTERNAL_SERVER_ERROR; }
    }
    if tx.commit().await.is_err() {
        StatusCode::INTERNAL_SERVER_ERROR
    } else {
        StatusCode::NO_CONTENT
    }
}

pub async fn bootstrap(state: &AppState) -> anyhow::Result<()> {
    if let Some(password) = &state.config.admin_password {
        let hash = hash_password(password)?;
        sqlx::query("INSERT INTO admins(id,username,password_hash) VALUES(1,?,?) ON CONFLICT(id) DO NOTHING").bind(&state.config.admin_username).bind(hash).execute(&state.db.0).await?;
    }
    let configured: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM admins")
        .fetch_one(&state.db.0)
        .await?;
    anyhow::ensure!(
        configured == 1,
        "administrator is not configured; set ADMIN_PASSWORD for bootstrap"
    );
    Ok(())
}
