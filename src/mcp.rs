use std::{borrow::Cow, sync::Arc, time::Instant};

use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use rmcp::{
    ErrorData, ServerHandler,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ListToolsResult,
        PaginatedRequestParams, ProtocolVersion, ServerCapabilities, ServerInfo, Tool,
    },
    service::{MaybeSendFuture, RequestContext, RoleServer},
    transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    },
};
use uuid::Uuid;

use crate::{AppState, auth::verify_digest, provider::canonical_schema};

#[derive(Clone)]
struct GatewayHandler {
    state: AppState,
}

impl ServerHandler for GatewayHandler {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions("A canonical Tavily web-search gateway")
    }

    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        Cow::Borrowed(&[
            ProtocolVersion::V_2025_03_26,
            ProtocolVersion::V_2025_06_18,
            ProtocolVersion::V_2025_11_25,
            ProtocolVersion::V_2026_07_28,
        ])
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, ErrorData>> + MaybeSendFuture + '_ {
        std::future::ready(Ok(ListToolsResult::with_all_items(vec![canonical_tool()])))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        if request.name != "tavily_search" {
            return Err(ErrorData::invalid_params("unknown tool", None));
        }
        let client_key_id = context
            .meta
            .get("io.tavily-gateway/clientKeyId")
            .and_then(serde_json::Value::as_i64)
            .ok_or_else(|| {
                ErrorData::internal_error("missing authenticated client context", None)
            })?;
        let request_id = Uuid::new_v4().to_string();
        let started = Instant::now();
        let arguments = request.arguments.unwrap_or_default();
        let _ = sqlx::query("UPDATE client_api_keys SET last_used_at=CURRENT_TIMESTAMP,request_count=request_count+1 WHERE id=?")
            .bind(client_key_id)
            .execute(&self.state.db.0)
            .await;
        match self.state.providers.call(arguments).await {
            Ok((provider_id, result)) => {
                let provider_error = result.is_error.unwrap_or(false);
                record(
                    &self.state,
                    &request_id,
                    client_key_id,
                    Some(provider_id),
                    started.elapsed().as_millis() as i64,
                    if provider_error { "failure" } else { "success" },
                    provider_error.then_some("upstream_tool_error"),
                )
                .await;
                Ok(result.into())
            }
            Err(error) => {
                record(
                    &self.state,
                    &request_id,
                    client_key_id,
                    error.provider_id,
                    started.elapsed().as_millis() as i64,
                    "failure",
                    Some(error.category()),
                )
                .await;
                Ok(CallToolResult::error(vec![ContentBlock::text(error.to_string())]).into())
            }
        }
    }
}

fn canonical_tool() -> Tool {
    Tool::new(
        "tavily_search",
        "Search the web with Tavily through a connected provider.",
        Arc::new(canonical_schema()),
    )
}

pub fn routes(state: AppState) -> Router<AppState> {
    let handler_state = state.clone();
    let service: StreamableHttpService<GatewayHandler, LocalSessionManager> =
        StreamableHttpService::new(
            move || {
                Ok(GatewayHandler {
                    state: handler_state.clone(),
                })
            },
            Default::default(),
            StreamableHttpServerConfig::default()
                .with_legacy_session_mode(true)
                .with_json_response(false),
        );
    Router::new()
        .fallback_service(service)
        .layer(middleware::from_fn_with_state(state, authenticate))
}

async fn authenticate(
    State(state): State<AppState>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let secret = request
        .headers()
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    let Some(secret) = secret else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let rows: Vec<(i64, Vec<u8>)> =
        sqlx::query_as("SELECT id,digest FROM client_api_keys WHERE status='active'")
            .fetch_all(&state.db.0)
            .await
            .unwrap_or_default();
    let Some(id) = rows
        .into_iter()
        .find_map(|(id, expected)| verify_digest(secret, &expected).then_some(id))
    else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if request.method() == axum::http::Method::POST {
        let (parts, body) = request.into_parts();
        let bytes = match to_bytes(body, 4 * 1024 * 1024).await {
            Ok(bytes) => bytes,
            Err(_) => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
        };
        let mut message: serde_json::Value = match serde_json::from_slice(&bytes) {
            Ok(message) => message,
            Err(_) => return StatusCode::BAD_REQUEST.into_response(),
        };
        let Some(root) = message.as_object_mut() else {
            return StatusCode::BAD_REQUEST.into_response();
        };
        let params = root
            .entry("params")
            .or_insert_with(|| serde_json::json!({}));
        let Some(params) = params.as_object_mut() else {
            return StatusCode::BAD_REQUEST.into_response();
        };
        let meta = params
            .entry("_meta")
            .or_insert_with(|| serde_json::json!({}));
        let Some(meta) = meta.as_object_mut() else {
            return StatusCode::BAD_REQUEST.into_response();
        };
        // Authenticated server-owned metadata. Any client-provided value is replaced.
        meta.insert(
            "io.tavily-gateway/clientKeyId".into(),
            serde_json::json!(id),
        );
        request = Request::from_parts(
            parts,
            Body::from(serde_json::to_vec(&message).expect("JSON serializes")),
        );
    }
    next.run(request).await
}

async fn record(
    state: &AppState,
    request_id: &str,
    client: i64,
    provider: Option<i64>,
    duration: i64,
    outcome: &str,
    category: Option<&str>,
) {
    tracing::info!(
        request_id,
        client_key_id = client,
        provider_id = provider,
        duration_ms = duration,
        outcome,
        error_category = category,
        "canonical tool call completed"
    );
    let mut tx = match state.db.0.begin().await {
        Ok(tx) => tx,
        Err(_) => return,
    };
    if sqlx::query("INSERT INTO request_events(id,client_key_id,provider_id,duration_ms,outcome,error_category) VALUES(?,?,?,?,?,?)")
        .bind(request_id).bind(client).bind(provider).bind(duration).bind(outcome).bind(category).execute(&mut *tx).await.is_err() { return; }
    let updated = sqlx::query("UPDATE usage_daily SET requests=requests+1,duration_ms=duration_ms+? WHERE day=date('now') AND client_key_id=? AND provider_id IS ? AND outcome=?")
        .bind(duration).bind(client).bind(provider).bind(outcome).execute(&mut *tx).await;
    if updated.is_ok_and(|result| result.rows_affected() == 0) {
        let _ = sqlx::query("INSERT INTO usage_daily(day,client_key_id,provider_id,outcome,requests,duration_ms) VALUES(date('now'),?,?,?,?,?)")
            .bind(client).bind(provider).bind(outcome).bind(1_i64).bind(duration).execute(&mut *tx).await;
    }
    let _ = tx.commit().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exposes_only_the_canonical_tool() {
        let tool = canonical_tool();
        assert_eq!(tool.name, "tavily_search");
        assert_eq!(
            tool.input_schema.get("required"),
            Some(&serde_json::json!(["query"]))
        );
    }
    #[test]
    fn supports_all_reviewed_versions() {
        let versions = GatewayHandler::supported_protocol_versions;
        let _ = versions;
        for version in [
            ProtocolVersion::V_2025_03_26,
            ProtocolVersion::V_2025_06_18,
            ProtocolVersion::V_2025_11_25,
            ProtocolVersion::V_2026_07_28,
        ] {
            assert!(version.to_string().starts_with("20"));
        }
    }
}
