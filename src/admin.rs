use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
    routing::{get, post, put},
};
use chrono::{Duration, Utc};
use rust_embed::RustEmbed;
use serde::Deserialize;
use serde_json::json;

use crate::{
    AppState,
    auth::{digest, hash_password, random_secret, verify_digest, verify_password},
    db::{GatewaySettings, NewProvider, ProviderUpdate},
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
    let db_ok = state.db.health_check().await.is_ok();
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
    let locked = match state.db.admin_login_is_locked(&input.username).await {
        Ok(locked) => locked,
        Err(error) => {
            tracing::error!(%error, "could not read admin login throttle");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    if locked {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({"error":"authentication temporarily unavailable"})),
        )
            .into_response();
    }
    let credentials = match state.db.admin_credentials().await {
        Ok(credentials) => credentials,
        Err(error) => {
            tracing::error!(%error, "could not read administrator credentials");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let authenticated = credentials.as_ref().is_some_and(|credentials| {
        credentials.username == input.username
            && verify_password(&input.password, &credentials.password_hash)
    });
    if !authenticated {
        if let Err(error) = state.db.record_admin_login_failure(&input.username).await {
            tracing::warn!(%error, "could not record admin login failure");
        }
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"invalid credentials"})),
        )
            .into_response();
    }
    if let Err(error) = state.db.clear_admin_login_failures(&input.username).await {
        tracing::warn!(%error, "could not clear admin login throttle");
    }
    let token = random_secret(32);
    let csrf = random_secret(32);
    let id = random_secret(16);
    let expires = Utc::now() + Duration::hours(8);
    let token_digest = digest(&token);
    let csrf_digest = digest(&csrf);
    if let Err(error) = state
        .db
        .create_admin_session(&id, &token_digest, &csrf_digest, expires)
        .await
    {
        tracing::error!(%error, "could not create admin session");
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
    if let Err(error) = state.db.delete_admin_session(&id).await {
        tracing::error!(%error, "could not delete admin session");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
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
    let session = match state.db.admin_session(&digest(token)).await {
        Ok(Some(session)) => session,
        Ok(None) => return None,
        Err(error) => {
            tracing::error!(%error, "could not authenticate admin session");
            return None;
        }
    };
    if write {
        let supplied = headers.get("x-csrf-token")?.to_str().ok()?;
        if !verify_digest(supplied, &session.csrf_digest) {
            return None;
        }
    }
    Some(session.id)
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
    match state.db.client_key_summaries().await {
        Ok(rows) => Json(rows).into_response(),
        Err(error) => {
            tracing::error!(%error, "could not list client API keys");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
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
    match state
        .db
        .create_client_key(&input.name, &prefix, &digest(&secret))
        .await
    {
        Ok(id) => (
            StatusCode::CREATED,
            Json(json!({"id":id,"prefix":prefix,"key":secret})),
        )
            .into_response(),
        Err(error) => {
            tracing::warn!(%error, "could not create client API key");
            StatusCode::BAD_REQUEST.into_response()
        }
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
    match state.db.revoke_client_key(id).await {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(error) => {
            tracing::error!(%error, "could not revoke client API key");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

async fn providers(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if admin_auth(&state, &headers, false).await.is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match state.db.provider_summaries().await {
        Ok(rows) => Json(rows).into_response(),
        Err(error) => {
            tracing::error!(%error, "could not list providers");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
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
    if !valid_provider(&input) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let Some(token) = input.token.filter(|token| !token.is_empty()) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let id = match state
        .db
        .create_provider(NewProvider {
            kind: input.kind,
            name: input.name,
            endpoint: input.endpoint,
            bearer_token: token,
            weight: input.weight,
            enabled: input.enabled,
            timeout_seconds: input.timeout_seconds,
        })
        .await
    {
        Ok(id) => id,
        Err(error) => {
            tracing::warn!(%error, "could not create provider");
            return StatusCode::BAD_REQUEST.into_response();
        }
    };
    let provider = match state.db.provider(id).await {
        Ok(Some(provider)) => provider,
        Ok(None) => {
            tracing::error!(provider_id = id, "created provider could not be reloaded");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        Err(error) => {
            tracing::error!(%error, provider_id = id, "could not reload created provider");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    if let Err(error) = state.providers.add(provider).await {
        tracing::warn!(%error, provider_id = id, "could not activate created provider");
        return StatusCode::BAD_GATEWAY.into_response();
    }
    (StatusCode::CREATED, Json(json!({"id":id}))).into_response()
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
    let updated = match state
        .db
        .update_provider(
            id,
            ProviderUpdate {
                kind: input.kind,
                name: input.name,
                endpoint: input.endpoint,
                bearer_token: input.token,
                weight: input.weight,
                enabled: input.enabled,
                timeout_seconds: input.timeout_seconds,
            },
        )
        .await
    {
        Ok(updated) => updated,
        Err(error) => {
            tracing::error!(%error, provider_id = id, "could not update provider");
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
    };
    if !updated {
        return StatusCode::NOT_FOUND;
    }
    let provider = match state.db.provider(id).await {
        Ok(Some(provider)) => provider,
        Ok(None) => return StatusCode::NOT_FOUND,
        Err(error) => {
            tracing::error!(%error, provider_id = id, "could not reload updated provider");
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
    };
    if let Err(error) = state.providers.update(provider).await {
        tracing::warn!(%error, provider_id = id, "could not apply provider update");
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
    match state.db.usage_overview().await {
        Ok(overview) => Json(overview).into_response(),
        Err(error) => {
            tracing::error!(%error, "could not load usage overview");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}
async fn requests(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if admin_auth(&state, &headers, false).await.is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match state.db.recent_requests().await {
        Ok(requests) => Json(requests).into_response(),
        Err(error) => {
            tracing::error!(%error, "could not load recent requests");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}
async fn settings(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if admin_auth(&state, &headers, false).await.is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match state.db.settings().await {
        Ok(settings) => Json(settings).into_response(),
        Err(error) => {
            tracing::error!(%error, "could not load gateway settings");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
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
    match state
        .db
        .update_settings(&GatewaySettings {
            default_timeout_seconds: input.default_timeout_seconds,
        })
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(error) => {
            tracing::error!(%error, "could not update gateway settings");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

pub async fn bootstrap(state: &AppState) -> anyhow::Result<()> {
    if let Some(password) = &state.config.admin_password {
        let hash = hash_password(password)?;
        state
            .db
            .insert_admin_if_missing(&state.config.admin_username, &hash)
            .await?;
    }
    let configured = state.db.admin_count().await?;
    anyhow::ensure!(
        configured == 1,
        "administrator is not configured; set ADMIN_PASSWORD for bootstrap"
    );
    Ok(())
}
