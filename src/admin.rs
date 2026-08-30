use axum::{
    Json, Router,
    extract::{FromRequestParts, Path, State},
    http::{HeaderMap, StatusCode, header, request::Parts},
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
    provider::ProviderMutationError,
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
    let readiness = state.gateway.readiness().await;
    (
        if readiness.ready {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        Json(json!({"ready":readiness.ready,"database":readiness.database})),
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
const ADMIN_LOGIN_THROTTLE_ID: &str = "administrator";

async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<Login>,
) -> impl IntoResponse {
    if !same_origin(&state, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if input.username.len() > 128 || input.password.len() > 1024 {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"invalid credentials"})),
        )
            .into_response();
    }
    let locked = match state
        .db
        .admin_login_is_locked(ADMIN_LOGIN_THROTTLE_ID)
        .await
    {
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
    let authenticated = match credentials {
        Some(credentials) => {
            let supplied_username = input.username;
            let supplied_password = input.password;
            match tokio::task::spawn_blocking(move || {
                let password_matches =
                    verify_password(&supplied_password, &credentials.password_hash);
                password_matches && credentials.username == supplied_username
            })
            .await
            {
                Ok(authenticated) => authenticated,
                Err(error) => {
                    tracing::error!(%error, "administrator password verification failed");
                    return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                }
            }
        }
        None => false,
    };
    if !authenticated {
        if let Err(error) = state
            .db
            .record_admin_login_failure(ADMIN_LOGIN_THROTTLE_ID)
            .await
        {
            tracing::warn!(%error, "could not record admin login failure");
        }
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"invalid credentials"})),
        )
            .into_response();
    }
    if let Err(error) = state
        .db
        .clear_admin_login_failures(ADMIN_LOGIN_THROTTLE_ID)
        .await
    {
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
    let secure = if state.web_security.production {
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

async fn session(_auth: AdminRead) -> impl IntoResponse {
    (StatusCode::OK, Json(json!({"authenticated":true})))
}
async fn logout(AdminWrite(id): AdminWrite, State(state): State<AppState>) -> impl IntoResponse {
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

struct AdminRead;
struct AdminWrite(String);

impl FromRequestParts<AppState> for AdminRead {
    type Rejection = StatusCode;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        admin_auth(state, &parts.headers, false)
            .await
            .map(|_| Self)
            .ok_or(StatusCode::UNAUTHORIZED)
    }
}

impl FromRequestParts<AppState> for AdminWrite {
    type Rejection = StatusCode;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        admin_auth(state, &parts.headers, true)
            .await
            .map(Self)
            .ok_or(StatusCode::UNAUTHORIZED)
    }
}

fn same_origin(state: &AppState, headers: &HeaderMap) -> bool {
    let Some(expected) = &state.web_security.public_origin else {
        return !state.web_security.production;
    };
    headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) == Some(expected.as_str())
}

async fn keys(_auth: AdminRead, State(state): State<AppState>) -> impl IntoResponse {
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
    _auth: AdminWrite,
    State(state): State<AppState>,
    Json(input): Json<Name>,
) -> impl IntoResponse {
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
    _auth: AdminWrite,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    match state.db.revoke_client_key(id).await {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(error) => {
            tracing::error!(%error, "could not revoke client API key");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

async fn providers(_auth: AdminRead, State(state): State<AppState>) -> impl IntoResponse {
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
    _auth: AdminWrite,
    State(state): State<AppState>,
    Json(input): Json<ProviderInput>,
) -> impl IntoResponse {
    let Some(token) = input.token.filter(|token| !token.is_empty()) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let id = match state
        .providers
        .create(NewProvider {
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
        Err(ProviderMutationError::Invalid(error)) => {
            tracing::warn!(error, "invalid provider configuration");
            return StatusCode::BAD_REQUEST.into_response();
        }
        Err(ProviderMutationError::Connection(error)) => {
            tracing::warn!(%error, "could not connect provider");
            return StatusCode::BAD_GATEWAY.into_response();
        }
        Err(ProviderMutationError::Storage(error)) => {
            tracing::error!(%error, "could not persist provider");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    (StatusCode::CREATED, Json(json!({"id":id}))).into_response()
}
async fn update_provider(
    _auth: AdminWrite,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(input): Json<ProviderInput>,
) -> impl IntoResponse {
    let updated = match state
        .providers
        .update_config(
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
        Err(ProviderMutationError::Invalid(error)) => {
            tracing::warn!(error, provider_id = id, "invalid provider configuration");
            return StatusCode::BAD_REQUEST;
        }
        Err(ProviderMutationError::Connection(error)) => {
            tracing::warn!(%error, provider_id = id, "could not connect updated provider");
            return StatusCode::BAD_GATEWAY;
        }
        Err(ProviderMutationError::Storage(error)) => {
            tracing::error!(%error, provider_id = id, "could not persist provider update");
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
    };
    if !updated {
        return StatusCode::NOT_FOUND;
    }
    StatusCode::NO_CONTENT
}
async fn overview(_auth: AdminRead, State(state): State<AppState>) -> impl IntoResponse {
    match state.db.usage_overview().await {
        Ok(overview) => Json(overview).into_response(),
        Err(error) => {
            tracing::error!(%error, "could not load usage overview");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}
async fn requests(_auth: AdminRead, State(state): State<AppState>) -> impl IntoResponse {
    match state.db.recent_requests().await {
        Ok(requests) => Json(requests).into_response(),
        Err(error) => {
            tracing::error!(%error, "could not load recent requests");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}
async fn settings(_auth: AdminRead, State(state): State<AppState>) -> impl IntoResponse {
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
    _auth: AdminWrite,
    State(state): State<AppState>,
    Json(input): Json<SettingsInput>,
) -> impl IntoResponse {
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

pub async fn bootstrap(
    db: &crate::db::Database,
    username: &str,
    password: Option<&str>,
) -> anyhow::Result<()> {
    if let Some(password) = password {
        let hash = hash_password(password)?;
        db.insert_admin_if_missing(username, &hash).await?;
    }
    let configured = db.admin_count().await?;
    anyhow::ensure!(
        configured == 1,
        "administrator is not configured; set ADMIN_PASSWORD for bootstrap"
    );
    Ok(())
}
