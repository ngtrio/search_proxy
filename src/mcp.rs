use std::borrow::Cow;

use crate::{AppState, auth::digest, provider::ToolCallError};
use axum::{
    Router,
    body::Body,
    extract::{Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use rmcp::{
    ErrorData, ServerHandler,
    model::{
        CallToolRequestParams, CallToolResponse, ListToolsResult, PaginatedRequestParams,
        ProtocolVersion, ServerCapabilities, ServerInfo,
    },
    service::{MaybeSendFuture, RequestContext, RoleServer},
    transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    },
};

#[derive(Clone)]
struct GatewayHandler {
    state: AppState,
}

#[derive(Clone, Copy)]
struct AuthenticatedClient(i64);

pub(crate) const SUPPORTED_PROTOCOL_VERSIONS: &[ProtocolVersion] = &[
    ProtocolVersion::V_2025_03_26,
    ProtocolVersion::V_2025_06_18,
    ProtocolVersion::V_2025_11_25,
    ProtocolVersion::V_2026_07_28,
];

impl ServerHandler for GatewayHandler {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions("A canonical gateway for the official Tavily MCP tool catalog")
    }

    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        Cow::Borrowed(SUPPORTED_PROTOCOL_VERSIONS)
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, ErrorData>> + MaybeSendFuture + '_ {
        std::future::ready(Ok(ListToolsResult::with_all_items(
            self.state.providers.tools().to_vec(),
        )))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let client_key_id = context
            .extensions
            .get::<axum::http::request::Parts>()
            .and_then(|parts| parts.extensions.get::<AuthenticatedClient>())
            .map(|client| client.0)
            .ok_or_else(|| {
                ErrorData::internal_error("missing authenticated client context", None)
            })?;
        let arguments = request.arguments.unwrap_or_default();
        match self
            .state
            .providers
            .call_tool(client_key_id, request.name.as_ref(), arguments)
            .await
        {
            Ok(result) => Ok(result.into()),
            Err(ToolCallError::UnknownTool) => Err(ErrorData::invalid_params("unknown tool", None)),
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
        .and_then(extract_bearer_token);
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
    request.extensions_mut().insert(AuthenticatedClient(id));
    next.run(request).await
}

fn extract_bearer_token(value: &str) -> Option<&str> {
    let mut parts = value.split_ascii_whitespace();
    let scheme = parts.next()?;
    let token = parts.next()?;
    (parts.next().is_none() && scheme.eq_ignore_ascii_case("bearer")).then_some(token)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{canonical_tool, canonical_tools};

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
        assert_eq!(
            SUPPORTED_PROTOCOL_VERSIONS,
            &[
                ProtocolVersion::V_2025_03_26,
                ProtocolVersion::V_2025_06_18,
                ProtocolVersion::V_2025_11_25,
                ProtocolVersion::V_2026_07_28,
            ]
        );
    }

    #[test]
    fn bearer_scheme_is_case_insensitive() {
        assert_eq!(extract_bearer_token("Bearer secret"), Some("secret"));
        assert_eq!(extract_bearer_token("bearer secret"), Some("secret"));
        assert_eq!(extract_bearer_token("BEARER secret"), Some("secret"));
        assert_eq!(extract_bearer_token("Basic secret"), None);
        assert_eq!(extract_bearer_token("Bearer"), None);
        assert_eq!(extract_bearer_token("Bearer secret extra"), None);
    }
}
