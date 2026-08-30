use std::sync::{Arc, Mutex};

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use http_body_util::BodyExt;
use rmcp::{
    ErrorData, ServerHandler,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ListToolsResult,
        PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
    },
    service::{MaybeSendFuture, RequestContext, RoleServer},
    transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    },
};
use serde_json::{Value, json};
use sqlx::SqlitePool;
use tavily_mcp_gateway::{
    AppState, admin, app,
    auth::digest,
    catalog::canonical_tools,
    db::{Database, NewProvider, ProviderUpdate},
    provider::ProviderManager,
};
use tower::ServiceExt;

#[derive(Clone, Default)]
struct MockSearchix {
    calls: Arc<std::sync::atomic::AtomicUsize>,
    fail: Arc<std::sync::atomic::AtomicBool>,
    advertise_all_tools: Arc<std::sync::atomic::AtomicBool>,
    last_tool: Arc<Mutex<Option<String>>>,
}

impl ServerHandler for MockSearchix {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
    }
    fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, ErrorData>> + MaybeSendFuture + '_ {
        let cursor = request.and_then(|request| request.cursor);
        std::future::ready(Ok(if cursor.is_none() {
            let mut page = ListToolsResult::with_all_items(vec![Tool::new(
                "unrelated",
                "not exposed",
                Arc::new(serde_json::Map::new()),
            )]);
            page.next_cursor = Some("page-2".into());
            page
        } else {
            let input_schema = serde_json::json!({
                "type": "object",
                "properties": {
                    "provider_specific": { "type": "boolean" }
                }
            })
            .as_object()
            .unwrap()
            .clone();
            ListToolsResult::with_all_items(
                canonical_tools()
                    .iter()
                    .filter(|tool| {
                        self.advertise_all_tools
                            .load(std::sync::atomic::Ordering::SeqCst)
                            || tool.name == "tavily_search"
                    })
                    .map(|tool| {
                        Tool::new(
                            format!("search_proxy_{}", tool.name),
                            "mock Tavily tool",
                            Arc::new(input_schema.clone()),
                        )
                    })
                    .collect(),
            )
        }))
    }
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        assert!(
            request.name.starts_with("search_proxy_tavily_"),
            "unexpected mapped tool: {}",
            request.name
        );
        *self.last_tool.lock().unwrap() = Some(request.name.to_string());
        assert!(
            request
                .arguments
                .as_ref()
                .and_then(|arguments| arguments.get("provider_specific"))
                .is_some(),
            "provider-specific arguments must pass through unchanged"
        );
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.fail.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(ErrorData::internal_error("scripted failure", None));
        }
        Ok(CallToolResult::success(vec![ContentBlock::text("mock result")]).into())
    }
}

async fn state() -> (AppState, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("temp dir");
    let database_url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("gateway.db").display()
    );
    let db = Database::connect(&database_url).await.expect("database");
    let providers = Arc::new(
        ProviderManager::new(db.clone())
            .await
            .expect("load providers"),
    );
    let state = AppState { db, providers };
    admin::bootstrap(&state.db, "admin", Some("correct horse battery staple"))
        .await
        .expect("bootstrap");
    (state, dir)
}

async fn body_json(response: axum::response::Response) -> Value {
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    serde_json::from_slice(&bytes).expect("json")
}

async fn body_sse_json(response: axum::response::Response) -> Value {
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let body = String::from_utf8(bytes.to_vec()).expect("utf-8 SSE body");
    let data = body
        .lines()
        .filter_map(|line| line.strip_prefix("data:").map(str::trim))
        .find(|data| !data.is_empty())
        .expect("SSE data frame");
    serde_json::from_str(data).expect("SSE JSON data")
}

#[tokio::test]
async fn mcp_requires_auth_and_negotiates_all_legacy_revisions() {
    let (state, _dir) = state().await;
    let secret = "tmg_test-secret";
    state
        .db
        .create_client_key("test", "tmg_test", &digest(secret))
        .await
        .unwrap();
    let gateway = app(state);
    let body_for = |version: &str| {
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":version,"capabilities":{},"clientInfo":{"name":"test","version":"1"}}}).to_string()
    };
    let unauthorized = gateway
        .clone()
        .oneshot(
            Request::post("/mcp")
                .header(header::HOST, "localhost")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body_for("2025-11-25")))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    for version in ["2025-03-26", "2025-06-18", "2025-11-25"] {
        let response = gateway
            .clone()
            .oneshot(
                Request::post("/mcp")
                    .header(header::HOST, "localhost")
                    .header(header::AUTHORIZATION, format!("Bearer {secret}"))
                    .header(header::ACCEPT, "application/json, text/event-stream")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body_for(version)))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{version}");
        assert!(response.headers().contains_key("mcp-session-id"));
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/event-stream"
        );
        let body = String::from_utf8(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec(),
        )
        .unwrap();
        assert!(body.contains(version));
    }
}

#[tokio::test]
async fn mcp_lists_and_accepts_the_complete_pinned_tavily_catalog() {
    let (state, _dir) = state().await;
    let secret = "tmg_catalog";
    state
        .db
        .create_client_key("catalog", "tmg_catalog", &digest(secret))
        .await
        .unwrap();
    let gateway = app(state);
    let authorization = format!("Bearer {secret}");
    let initialized = gateway
        .clone()
        .oneshot(
            Request::post("/mcp")
                .header(header::HOST, "localhost")
                .header(header::AUTHORIZATION, &authorization)
                .header(header::ACCEPT, "application/json, text/event-stream")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let session = initialized
        .headers()
        .get("mcp-session-id")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let request = |body: Value| {
        Request::post("/mcp")
            .header(header::HOST, "localhost")
            .header(header::AUTHORIZATION, &authorization)
            .header("Mcp-Session-Id", &session)
            .header("Mcp-Protocol-Version", "2025-11-25")
            .header(header::ACCEPT, "application/json, text/event-stream")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    };
    let _ = gateway
        .clone()
        .oneshot(request(json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        })))
        .await
        .unwrap();

    let listed = gateway
        .clone()
        .oneshot(request(json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {}
        })))
        .await
        .unwrap();
    let listed = body_sse_json(listed).await;
    let names = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        [
            "tavily_search",
            "tavily_extract",
            "tavily_crawl",
            "tavily_map",
            "tavily_research",
        ]
    );
    assert_eq!(
        listed["result"]["tools"][4]["inputSchema"]["required"],
        json!(["input"])
    );

    let called = gateway
        .oneshot(request(json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {"name": "tavily_research", "arguments": {"input": "test"}}
        })))
        .await
        .unwrap();
    let called = body_sse_json(called).await;
    assert!(
        called["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("no connected provider")
    );
}

#[tokio::test]
async fn modern_discovery_is_stateless() {
    let (state, _dir) = state().await;
    let secret = "tmg_modern";
    state
        .db
        .create_client_key("modern", "tmg_modern", &digest(secret))
        .await
        .unwrap();
    let body = json!({"jsonrpc":"2.0","id":1,"method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientInfo":{"name":"test","version":"1"},"io.modelcontextprotocol/clientCapabilities":{}}}}).to_string();
    let response = app(state)
        .oneshot(
            Request::post("/mcp")
                .header(header::HOST, "localhost")
                .header(header::AUTHORIZATION, format!("Bearer {secret}"))
                .header(header::ACCEPT, "application/json, text/event-stream")
                .header(header::CONTENT_TYPE, "application/json")
                .header("Mcp-Method", "server/discover")
                .header("Mcp-Protocol-Version", "2026-07-28")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(!response.headers().contains_key("mcp-session-id"));
}

#[tokio::test]
async fn admin_csrf_write_only_tokens() {
    let (state, _dir) = state().await;
    let gateway = app(state.clone());
    let login_body =
        json!({"username":"admin","password":"correct horse battery staple"}).to_string();
    let response = gateway
        .clone()
        .oneshot(
            Request::post("/admin/api/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(login_body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let cookies = response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|v| {
            let value = v.to_str().unwrap();
            assert!(value.contains("; Secure"));
            value.split(';').next().unwrap().to_owned()
        })
        .collect::<Vec<_>>();
    assert_eq!(cookies.len(), 2);
    let cookie = cookies.join("; ");
    let csrf = cookies
        .iter()
        .find_map(|value| value.strip_prefix("gateway_csrf="))
        .unwrap();
    let create = json!({"kind":"searchix","name":"Searchix","endpoint":"https://example.test/mcp","token":"upstream-secret","weight":1,"enabled":false}).to_string();
    let no_csrf = gateway
        .clone()
        .oneshot(
            Request::post("/admin/api/providers")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::COOKIE, &cookie)
                .body(Body::from(create.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(no_csrf.status(), StatusCode::UNAUTHORIZED);
    let created = gateway
        .clone()
        .oneshot(
            Request::post("/admin/api/providers")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::COOKIE, &cookie)
                .header("x-csrf-token", csrf)
                .body(Body::from(create))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let listed = gateway
        .clone()
        .oneshot(
            Request::get("/admin/api/providers")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let listed = body_json(listed).await;
    let encoded = listed.to_string();
    assert!(!encoded.contains("upstream-secret"));
    assert!(!encoded.contains("bearer_token"));
    assert_eq!(listed[0]["token_configured"], true);
}

#[tokio::test]
async fn provider_schema_has_no_probe_or_cooldown_state() {
    let (_state, dir) = state().await;
    let pool = SqlitePool::connect(&format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("gateway.db").display()
    ))
    .await
    .unwrap();
    let provider_columns: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('providers')")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert!(
        !provider_columns
            .iter()
            .any(|column| column == "base_cooldown_seconds")
    );
    assert!(
        !provider_columns
            .iter()
            .any(|column| column == "timeout_seconds")
    );
    let probe_tables: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='provider_probe_results'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(probe_tables, 0);
}

#[tokio::test]
async fn settings_table_is_removed() {
    let (_state, dir) = state().await;
    let pool = SqlitePool::connect(&format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("gateway.db").display()
    ))
    .await
    .unwrap();
    let settings_table: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='settings'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(settings_table, 0);
}

#[tokio::test]
async fn tool_calls_write_metadata_and_daily_aggregates_only() {
    let (state, dir) = state().await;
    let pool = SqlitePool::connect(&format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("gateway.db").display()
    ))
    .await
    .unwrap();
    let secret = "tmg_usage";
    state
        .db
        .create_client_key("usage", "tmg_usage", &digest(secret))
        .await
        .unwrap();
    let gateway = app(state.clone());
    let authorization = format!("Bearer {secret}");
    let initialize = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}).to_string();
    let initialized = gateway
        .clone()
        .oneshot(
            Request::post("/mcp")
                .header(header::HOST, "localhost")
                .header(header::AUTHORIZATION, &authorization)
                .header(header::ACCEPT, "application/json, text/event-stream")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(initialize))
                .unwrap(),
        )
        .await
        .unwrap();
    let session = initialized
        .headers()
        .get("mcp-session-id")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let notification = json!({"jsonrpc":"2.0","method":"notifications/initialized"}).to_string();
    let _ = gateway
        .clone()
        .oneshot(
            Request::post("/mcp")
                .header(header::HOST, "localhost")
                .header(header::AUTHORIZATION, &authorization)
                .header("Mcp-Session-Id", &session)
                .header("Mcp-Protocol-Version", "2025-11-25")
                .header(header::ACCEPT, "application/json, text/event-stream")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(notification))
                .unwrap(),
        )
        .await
        .unwrap();
    let call = json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"tavily_search","arguments":{"query":"must never persist"}}}).to_string();
    let response = gateway
        .clone()
        .oneshot(
            Request::post("/mcp")
                .header(header::HOST, "localhost")
                .header(header::AUTHORIZATION, &authorization)
                .header("Mcp-Session-Id", &session)
                .header("Mcp-Protocol-Version", "2025-11-25")
                .header(header::ACCEPT, "application/json, text/event-stream")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(call))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let response_body = String::from_utf8(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    assert!(
        response_body.contains("no connected provider"),
        "unexpected MCP response: {response_body}"
    );
    let second_call = json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"tavily_search","arguments":{"query":"must never persist"}}}).to_string();
    let second = gateway
        .oneshot(
            Request::post("/mcp")
                .header(header::HOST, "localhost")
                .header(header::AUTHORIZATION, &authorization)
                .header("Mcp-Session-Id", &session)
                .header("Mcp-Protocol-Version", "2025-11-25")
                .header(header::ACCEPT, "application/json, text/event-stream")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(second_call))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::OK);
    let second_body = String::from_utf8(
        second
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    assert!(
        second_body.contains("no connected provider"),
        "unexpected MCP response: {second_body}"
    );
    let events: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM request_events")
        .fetch_one(&pool)
        .await
        .unwrap();
    let aggregates: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM usage_daily")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!((events, aggregates), (2, 1));
    let aggregate_requests: i64 = sqlx::query_scalar("SELECT requests FROM usage_daily")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(aggregate_requests, 2);
    let calls: i64 =
        sqlx::query_scalar("SELECT request_count FROM client_api_keys WHERE name='usage'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        calls, 2,
        "MCP lifecycle traffic must not count as tool calls"
    );
    let columns: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('request_events')")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert!(
        !columns
            .iter()
            .any(|column| ["query", "arguments", "results", "content"].contains(&column.as_str()))
    );
}

#[tokio::test]
async fn provider_startup_connect_paginates_maps_and_routes_all_tools() {
    let mock = MockSearchix::default();
    mock.advertise_all_tools
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let service: StreamableHttpService<MockSearchix, LocalSessionManager> =
        StreamableHttpService::new(
            {
                let mock = mock.clone();
                move || Ok(mock.clone())
            },
            Default::default(),
            StreamableHttpServerConfig::default()
                .with_legacy_session_mode(false)
                .with_json_response(false),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, axum::Router::new().nest_service("/mcp", service)).await;
    });
    let (state, _dir) = state().await;
    let endpoint = format!("http://{address}/mcp");
    let provider_id = state
        .providers
        .create(NewProvider {
            kind: "searchix".into(),
            name: "mock".into(),
            endpoint: endpoint.clone(),
            bearer_token: "test".into(),
            weight: 1,
            enabled: true,
        })
        .await
        .unwrap();
    for tool in state.providers.tools() {
        let mut arguments = serde_json::Map::new();
        arguments.insert("provider_specific".into(), json!(true));
        let (_, result) = state
            .providers
            .call(tool.name.as_ref(), arguments)
            .await
            .unwrap();
        assert_eq!(result.is_error, Some(false));
        assert_eq!(
            mock.last_tool.lock().unwrap().as_deref(),
            Some(format!("search_proxy_{}", tool.name).as_str())
        );
    }
    assert_eq!(
        mock.calls.load(std::sync::atomic::Ordering::SeqCst),
        state.providers.tools().len()
    );
    mock.fail.store(true, std::sync::atomic::Ordering::SeqCst);
    let mut arguments = serde_json::Map::new();
    arguments.insert("provider_specific".into(), json!(false));
    let failure = state
        .providers
        .call("tavily_extract", arguments)
        .await
        .unwrap_err();
    assert_eq!(failure.provider_id, Some(provider_id));
    assert_eq!(
        mock.calls.load(std::sync::atomic::Ordering::SeqCst),
        state.providers.tools().len() + 1
    );
    assert_eq!(
        mock.last_tool.lock().unwrap().as_deref(),
        Some("search_proxy_tavily_extract")
    );
    mock.fail.store(false, std::sync::atomic::Ordering::SeqCst);
    let mut arguments = serde_json::Map::new();
    arguments.insert("provider_specific".into(), json!(true));
    let (_, recovered) = state
        .providers
        .call("tavily_extract", arguments)
        .await
        .unwrap();
    assert_eq!(recovered.is_error, Some(false));

    let closed_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let closed_address = closed_listener.local_addr().unwrap();
    drop(closed_listener);
    let failed_update = state
        .providers
        .update_config(
            provider_id,
            ProviderUpdate {
                kind: "searchix".into(),
                name: "unreachable replacement".into(),
                endpoint: format!("http://{closed_address}/mcp"),
                bearer_token: None,
                weight: 2,
                enabled: true,
            },
        )
        .await;
    assert!(failed_update.is_err());
    let stored = state.db.provider(provider_id).await.unwrap().unwrap();
    assert_eq!(stored.endpoint, endpoint);
    assert_eq!(stored.weight, 1);
    let mut arguments = serde_json::Map::new();
    arguments.insert("provider_specific".into(), json!(true));
    assert!(
        state
            .providers
            .call("tavily_search", arguments)
            .await
            .is_ok()
    );
    task.abort();
}

#[tokio::test]
async fn failed_provider_connections_do_not_poison_persistence_or_startup() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);

    let (state, _dir) = state().await;
    let unreachable = NewProvider {
        kind: "searchix".into(),
        name: "unreachable".into(),
        endpoint: format!("http://{address}/mcp"),
        bearer_token: "test".into(),
        weight: 1,
        enabled: true,
    };
    assert!(state.providers.create(unreachable.clone()).await.is_err());
    assert!(state.db.provider_summaries().await.unwrap().is_empty());

    state.db.create_provider(unreachable).await.unwrap();
    let loaded = ProviderManager::new(state.db.clone())
        .await
        .expect("an unavailable provider must not prevent the control plane from starting");
    let failure = loaded
        .call("tavily_search", serde_json::Map::new())
        .await
        .unwrap_err();
    assert_eq!(failure.provider_id, None);
}

#[tokio::test]
async fn enabled_provider_creation_connects_before_activation() {
    let mock = MockSearchix::default();
    let service: StreamableHttpService<MockSearchix, LocalSessionManager> =
        StreamableHttpService::new(
            {
                let mock = mock.clone();
                move || Ok(mock.clone())
            },
            Default::default(),
            StreamableHttpServerConfig::default()
                .with_legacy_session_mode(false)
                .with_json_response(false),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, axum::Router::new().nest_service("/mcp", service)).await;
    });

    let (state, _dir) = state().await;
    let gateway = app(state.clone());
    let login = gateway
        .clone()
        .oneshot(
            Request::post("/admin/api/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"username":"admin","password":"correct horse battery staple"})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let cookies = login
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|value| {
            value
                .to_str()
                .unwrap()
                .split(';')
                .next()
                .unwrap()
                .to_owned()
        })
        .collect::<Vec<_>>();
    let csrf = cookies
        .iter()
        .find_map(|value| value.strip_prefix("gateway_csrf="))
        .unwrap();
    let created = gateway
        .oneshot(
            Request::post("/admin/api/providers")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::COOKIE, cookies.join("; "))
                .header("x-csrf-token", csrf)
                .body(Body::from(
                    json!({
                        "kind":"searchix",
                        "name":"mock",
                        "endpoint":format!("http://{address}/mcp"),
                        "token":"test",
                        "weight":1,
                        "enabled":true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let created = body_json(created).await;
    assert!(created["id"].is_number());
    let mut arguments = serde_json::Map::new();
    arguments.insert("provider_specific".into(), json!(true));
    let (_, result) = state
        .providers
        .call("tavily_search", arguments)
        .await
        .unwrap();
    assert_eq!(result.is_error, Some(false));
    let unsupported = state
        .providers
        .call("tavily_extract", serde_json::Map::new())
        .await
        .unwrap_err();
    assert_eq!(unsupported.provider_id, None);
    assert_eq!(
        mock.calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "a provider that did not advertise a tool must not receive that call"
    );
    task.abort();
}
