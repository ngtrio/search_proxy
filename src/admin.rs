use axum::{
    Json, Router,
    extract::{FromRequestParts, Path, State},
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header, request::Parts},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tower_http::cors::{AllowOrigin, CorsLayer};

use crate::{
    AdminConfig, AppState,
    auth::{digest, hash_password, random_secret, verify_digest, verify_password},
    db::{NewProvider, ProviderKind, ProviderSummary, ProviderUpdate},
    provider::{ProviderMutationError, ProviderToolMapping},
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/session", get(session))
        .route("/login", post(login))
        .route("/logout", post(logout))
        .route("/keys", get(keys).post(create_key))
        .route("/providers", get(providers).post(create_provider))
        .route("/providers/{id}", put(update_provider))
        .route("/overview", get(overview))
        .route("/requests", get(requests))
}

pub fn cors_layer(config: &AdminConfig) -> CorsLayer {
    let origins = config
        .cors_origins
        .iter()
        .filter_map(|origin| HeaderValue::from_str(origin).ok())
        .collect::<Vec<_>>();

    CorsLayer::new()
        .allow_origin(AllowOrigin::list(origins))
        .allow_credentials(true)
        .allow_methods([Method::GET, Method::POST, Method::PUT])
        .allow_headers([
            header::CONTENT_TYPE,
            HeaderName::from_static("x-csrf-token"),
        ])
}

fn api_error(status: StatusCode, message: &'static str) -> Response {
    (status, Json(json!({"error": message}))).into_response()
}

#[derive(Deserialize)]
struct Login {
    username: String,
    password: String,
}

async fn login(State(state): State<AppState>, Json(input): Json<Login>) -> impl IntoResponse {
    if input.username.len() > 128 || input.password.len() > 1024 {
        return api_error(StatusCode::UNAUTHORIZED, "invalid credentials");
    }
    let credentials = match state.db.admin_credentials().await {
        Ok(credentials) => credentials,
        Err(error) => {
            tracing::error!(%error, "could not read administrator credentials");
            return api_error(StatusCode::INTERNAL_SERVER_ERROR, "internal server error");
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
                    return api_error(StatusCode::INTERNAL_SERVER_ERROR, "internal server error");
                }
            }
        }
        None => false,
    };
    if !authenticated {
        return api_error(StatusCode::UNAUTHORIZED, "invalid credentials");
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
    let cookie_attributes = cookie_attributes(&state.admin);
    let cookie =
        format!("gateway_session={token}; Path=/; HttpOnly; {cookie_attributes}; Max-Age=28800");
    let csrf_cookie = format!("gateway_csrf={csrf}; Path=/; {cookie_attributes}; Max-Age=28800");
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
        return api_error(StatusCode::INTERNAL_SERVER_ERROR, "internal server error");
    }
    let mut response = (StatusCode::NO_CONTENT,).into_response();
    let cookie_attributes = cookie_attributes(&state.admin);
    response.headers_mut().append(
        header::SET_COOKIE,
        format!("gateway_session=; Path=/; HttpOnly; {cookie_attributes}; Max-Age=0")
            .parse()
            .expect("valid cookie"),
    );
    response.headers_mut().append(
        header::SET_COOKIE,
        format!("gateway_csrf=; Path=/; {cookie_attributes}; Max-Age=0")
            .parse()
            .expect("valid cookie"),
    );
    response
}

fn cookie_attributes(config: &AdminConfig) -> &'static str {
    if config.secure_cookies {
        "SameSite=None; Secure"
    } else {
        "SameSite=Lax"
    }
}

async fn admin_auth(state: &AppState, headers: &HeaderMap, write: bool) -> Option<String> {
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
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        admin_auth(state, &parts.headers, false)
            .await
            .map(|_| Self)
            .ok_or_else(|| api_error(StatusCode::UNAUTHORIZED, "authentication required"))
    }
}

impl FromRequestParts<AppState> for AdminWrite {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        admin_auth(state, &parts.headers, true)
            .await
            .map(Self)
            .ok_or_else(|| api_error(StatusCode::UNAUTHORIZED, "authentication required"))
    }
}

async fn keys(_auth: AdminRead, State(state): State<AppState>) -> impl IntoResponse {
    match state.db.client_key_summaries().await {
        Ok(rows) => Json(rows).into_response(),
        Err(error) => {
            tracing::error!(%error, "could not list client API keys");
            api_error(StatusCode::INTERNAL_SERVER_ERROR, "internal server error")
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
        return api_error(StatusCode::BAD_REQUEST, "name is required");
    }
    let secret = format!("tmg_{}", random_secret(32));
    let prefix = secret.chars().take(12).collect::<String>();
    match state
        .db
        .create_client_key(&input.name, &prefix, &digest(&secret), &secret)
        .await
    {
        Ok(id) => (
            StatusCode::CREATED,
            Json(json!({"id":id,"prefix":prefix,"key":secret})),
        )
            .into_response(),
        Err(error) => {
            tracing::warn!(%error, "could not create client API key");
            api_error(StatusCode::BAD_REQUEST, "could not create client API key")
        }
    }
}
#[derive(Serialize)]
struct ProviderView {
    #[serde(flatten)]
    provider: ProviderSummary,
    connected: bool,
    tool_mappings: Vec<ProviderToolMapping>,
}

async fn providers(_auth: AdminRead, State(state): State<AppState>) -> impl IntoResponse {
    match state.db.provider_summaries().await {
        Ok(rows) => Json(
            rows.into_iter()
                .map(|provider| {
                    let tool_mappings = state.providers.tool_mappings(provider.id);
                    ProviderView {
                        connected: tool_mappings.is_some(),
                        tool_mappings: tool_mappings.unwrap_or_default(),
                        provider,
                    }
                })
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(error) => {
            tracing::error!(%error, "could not list providers");
            api_error(StatusCode::INTERNAL_SERVER_ERROR, "internal server error")
        }
    }
}
#[derive(Deserialize)]
struct ProviderInput {
    kind: ProviderKind,
    name: String,
    endpoint: String,
    token: Option<String>,
    weight: i64,
    enabled: bool,
}
async fn create_provider(
    _auth: AdminWrite,
    State(state): State<AppState>,
    Json(input): Json<ProviderInput>,
) -> impl IntoResponse {
    let Some(token) = input.token.filter(|token| !token.is_empty()) else {
        return api_error(StatusCode::BAD_REQUEST, "token is required");
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
        })
        .await
    {
        Ok(id) => id,
        Err(ProviderMutationError::Invalid(error)) => {
            tracing::warn!(error, "invalid provider configuration");
            return api_error(StatusCode::BAD_REQUEST, "invalid provider configuration");
        }
        Err(ProviderMutationError::Connection(error)) => {
            tracing::warn!(%error, "could not connect provider");
            return api_error(StatusCode::BAD_GATEWAY, "could not connect provider");
        }
        Err(ProviderMutationError::Storage(error)) => {
            tracing::error!(%error, "could not persist provider");
            return api_error(StatusCode::INTERNAL_SERVER_ERROR, "internal server error");
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
            },
        )
        .await
    {
        Ok(updated) => updated,
        Err(ProviderMutationError::Invalid(error)) => {
            tracing::warn!(error, provider_id = id, "invalid provider configuration");
            return api_error(StatusCode::BAD_REQUEST, "invalid provider configuration");
        }
        Err(ProviderMutationError::Connection(error)) => {
            tracing::warn!(%error, provider_id = id, "could not connect updated provider");
            return api_error(StatusCode::BAD_GATEWAY, "could not connect provider");
        }
        Err(ProviderMutationError::Storage(error)) => {
            tracing::error!(%error, provider_id = id, "could not persist provider update");
            return api_error(StatusCode::INTERNAL_SERVER_ERROR, "internal server error");
        }
    };
    if !updated {
        return api_error(StatusCode::NOT_FOUND, "provider not found");
    }
    StatusCode::NO_CONTENT.into_response()
}
async fn overview(_auth: AdminRead, State(state): State<AppState>) -> impl IntoResponse {
    match state.db.usage_overview().await {
        Ok(overview) => Json(overview).into_response(),
        Err(error) => {
            tracing::error!(%error, "could not load usage overview");
            api_error(StatusCode::INTERNAL_SERVER_ERROR, "internal server error")
        }
    }
}
async fn requests(_auth: AdminRead, State(state): State<AppState>) -> impl IntoResponse {
    match state.db.recent_requests().await {
        Ok(requests) => Json(requests).into_response(),
        Err(error) => {
            tracing::error!(%error, "could not load recent requests");
            api_error(StatusCode::INTERNAL_SERVER_ERROR, "internal server error")
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
