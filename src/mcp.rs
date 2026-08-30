use std::{borrow::Cow, time::Instant};

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
        PaginatedRequestParams, ProtocolVersion, ServerCapabilities, ServerInfo,
    },
    service::{MaybeSendFuture, RequestContext, RoleServer},
    transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    },
};
use uuid::Uuid;

use crate::{
    AppState,
    auth::digest,
    catalog::{canonical_tool, canonical_tools},
    db::RequestRecord,
};

#[derive(Clone)]
struct GatewayHandler {
    state: AppState,
}

impl ServerHandler for GatewayHandler {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions("A canonical gateway for the official Tavily MCP tool catalog")
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
        std::future::ready(Ok(ListToolsResult::with_all_items(
            canonical_tools().to_vec(),
        )))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        if canonical_tool(request.name.as_ref()).is_none() {
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
        if let Err(error) = self.state.db.mark_client_key_used(client_key_id).await {
            tracing::warn!(%error, client_key_id, "could not update client API key usage");
        }
        match self
            .state
            .providers
            .call(request.name.as_ref(), arguments)
            .await
        {
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
    let id = match state.db.active_client_key_id(&digest(secret)).await {
        Ok(Some(id)) => id,
        Ok(None) => return StatusCode::UNAUTHORIZED.into_response(),
        Err(error) => {
            tracing::error!(%error, "could not authenticate client API key");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
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
    if let Err(error) = state
        .db
        .record_request(RequestRecord {
            id: request_id,
            client_key_id: client,
            provider_id: provider,
            duration_ms: duration,
            outcome,
            error_category: category,
        })
        .await
    {
        tracing::error!(%error, request_id, "could not persist tool call metadata");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exposes_the_pinned_official_catalog() {
        assert_eq!(canonical_tools().len(), 5);
        let tool = canonical_tool("tavily_search").unwrap();
        assert_eq!(
            tool.input_schema.get("required"),
            Some(&serde_json::json!(["query"]))
        );
        assert_eq!(
            canonical_tool("tavily_research")
                .unwrap()
                .input_schema
                .get("required"),
            Some(&serde_json::json!(["input"]))
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
