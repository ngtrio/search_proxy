use std::sync::{Arc, Mutex};

use axum::{
    Json,
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, Request, StatusCode, header},
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
    AdminConfig, AppState, McpConfig, admin, app,
    auth::digest,
    catalog::canonical_tools,
    db::{Database, NewProvider, ProviderKind, ProviderUpdate, RequestRecord},
    provider::ProviderManager,
};
use tower::ServiceExt;

#[derive(Clone, Default)]
struct MockSearchix {
    calls: Arc<std::sync::atomic::AtomicUsize>,
    fail: Arc<std::sync::atomic::AtomicBool>,
    advertise_all_tools: Arc<std::sync::atomic::AtomicBool>,
    return_pending_research: Arc<std::sync::atomic::AtomicBool>,
    last_tool: Arc<Mutex<Option<String>>>,
}

#[derive(Clone, Default)]
struct MockResearchApi {
    calls: Arc<std::sync::atomic::AtomicUsize>,
    pending_responses: Arc<std::sync::atomic::AtomicUsize>,
    saw_bearer: Arc<std::sync::atomic::AtomicBool>,
}

async fn mock_research_status(
    State(api): State<MockResearchApi>,
    Path(request_id): Path<String>,
    headers: HeaderMap,
) -> (StatusCode, Json<Value>) {
    let call = api.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    api.saw_bearer.store(
        headers
            .get(header::AUTHORIZATION)
            .is_some_and(|value| value == "Bearer test"),
        std::sync::atomic::Ordering::SeqCst,
    );
    if call
        < api
            .pending_responses
            .load(std::sync::atomic::Ordering::SeqCst)
    {
        return (
            StatusCode::ACCEPTED,
            Json(json!({
                "request_id": request_id,
                "status": "in_progress"
            })),
        );
    }
    (
        StatusCode::OK,
        Json(json!({
            "request_id": request_id,
            "status": "completed",
            "content": "finished research",
            "sources": [{"url": "https://example.test/source"}]
        })),
    )
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
        if request.name.ends_with("tavily_research")
            && self
                .return_pending_research
                .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Ok(CallToolResult::success(vec![ContentBlock::text(
                json!({
                    "request_id": "research-123",
                    "status": "pending"
                })
                .to_string(),
            )])
            .into());
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
    let state = AppState {
        db,
        providers,
        admin: AdminConfig::default(),
        mcp: McpConfig::default(),
    };
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

#[tokio::test]
async fn public_metrics_are_aggregated_validated_and_anonymous() {
    let (state, _dir) = state().await;
    let key_id = state
        .db
        .create_client_key(
            "metrics-test",
            "tmg_metric",
            &digest("metrics-secret"),
            "metrics-secret",
        )
        .await
        .unwrap();
    for (index, duration_ms) in [10, 20, 30, 40].into_iter().enumerate() {
        let request_id = format!("metric-{index}");
        state
            .db
            .record_request(RequestRecord {
                id: &request_id,
                client_key_id: key_id,
                provider_id: None,
                duration_ms,
                outcome: if index == 3 { "error" } else { "success" },
                error_category: (index == 3).then_some("private-error"),
            })
            .await
            .unwrap();
    }

    let gateway = app(state);
    let response = gateway
        .clone()
        .oneshot(
            Request::get("/api/metrics?window=1h")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let metrics = body_json(response).await;
    assert_eq!(metrics["window"], "1h");
    assert_eq!(metrics["bucket_seconds"], 60);
    assert_eq!(metrics["summary"]["requests"]["value"], 4);
    assert_eq!(metrics["summary"]["success_rate"]["value"], 75.0);
    assert_eq!(metrics["summary"]["p50_ms"]["value"], 20);
    assert_eq!(metrics["summary"]["p95_ms"]["value"], 40);
    assert_eq!(metrics["series"].as_array().unwrap().len(), 60);
    assert_eq!(metrics["activity"].as_array().unwrap().len(), 288);
    assert_eq!(
        metrics["latency_distribution"].as_array().unwrap().len(),
        24
    );
    let serialized = metrics.to_string();
    for sensitive in [
        "metrics-secret",
        "metrics-test",
        "private-error",
        "provider",
    ] {
        assert!(!serialized.contains(sensitive));
    }

    let invalid = gateway
        .oneshot(
            Request::get("/api/metrics?window=2h")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn all_metrics_include_full_history_without_a_previous_period() {
    let (state, dir) = state().await;
    let key_id = state
        .db
        .create_client_key(
            "all-metrics",
            "tmg_all",
            &digest("all-secret"),
            "all-secret",
        )
        .await
        .unwrap();
    state
        .db
        .record_request(RequestRecord {
            id: "all-history-request",
            client_key_id: key_id,
            provider_id: None,
            duration_ms: 125,
            outcome: "success",
            error_category: None,
        })
        .await
        .unwrap();

    let database_url = format!("sqlite://{}", dir.path().join("gateway.db").display());
    let pool = SqlitePool::connect(&database_url).await.unwrap();
    sqlx::query("UPDATE request_events SET started_at = datetime('now', '-45 days') WHERE id = ?")
        .bind("all-history-request")
        .execute(&pool)
        .await
        .unwrap();

    let response = app(state)
        .oneshot(
            Request::get("/api/metrics?window=all")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let metrics = body_json(response).await;
    assert_eq!(metrics["window"], "all");
    assert_eq!(metrics["bucket_seconds"], 21_600);
    assert_eq!(metrics["summary"]["requests"]["value"], 1);
    assert_eq!(metrics["summary"]["success_rate"]["value"], 100.0);
    assert_eq!(metrics["summary"]["p50_ms"]["value"], 125);
    assert!(metrics["summary"]["requests"]["change"].is_null());
    assert!(metrics["summary"]["success_rate"]["change"].is_null());
    assert!(metrics["summary"]["p50_ms"]["change"].is_null());
    assert!(metrics["series"].as_array().unwrap().len() <= 300);
}

#[tokio::test]
async fn all_metrics_use_a_safe_range_when_history_is_empty() {
    let (state, _dir) = state().await;
    let response = app(state)
        .oneshot(
            Request::get("/api/metrics?window=all")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let metrics = body_json(response).await;
    assert_eq!(metrics["bucket_seconds"], 60);
    assert_eq!(metrics["summary"]["requests"]["value"], 0);
    assert_eq!(metrics["series"].as_array().unwrap().len(), 60);
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
        .create_client_key("test", "tmg_test", &digest(secret), secret)
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
async fn mcp_accepts_only_configured_host_headers() {
    let (mut state, _dir) = state().await;
    let secret = "tmg_allowed-host";
    state
        .db
        .create_client_key("allowed-host", "tmg_allowed-host", &digest(secret), secret)
        .await
        .unwrap();
    state.mcp.allowed_hosts = vec!["mcp.example.com".into()];
    let gateway = app(state);
    let request = |host: &'static str| {
        Request::post("/mcp")
            .header(header::HOST, host)
            .header(header::AUTHORIZATION, format!("Bearer {secret}"))
            .header(header::ACCEPT, "application/json, text/event-stream")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}).to_string(),
            ))
            .unwrap()
    };

    let allowed = gateway
        .clone()
        .oneshot(request("mcp.example.com"))
        .await
        .unwrap();
    assert_eq!(allowed.status(), StatusCode::OK);

    let rejected = gateway
        .oneshot(request("attacker.example.com"))
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn mcp_lists_and_accepts_the_complete_pinned_tavily_catalog() {
    let (state, _dir) = state().await;
    let secret = "tmg_catalog";
    state
        .db
        .create_client_key("catalog", "tmg_catalog", &digest(secret), secret)
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
async fn mcp_2026_tools_list_includes_required_cache_hints() {
    let (state, _dir) = state().await;
    let secret = "tmg_cache-hints";
    state
        .db
        .create_client_key("cache-hints", "tmg_cache", &digest(secret), secret)
        .await
        .unwrap();

    let response = app(state)
        .oneshot(
            Request::post("/mcp")
                .header(header::HOST, "localhost")
                .header(header::AUTHORIZATION, format!("Bearer {secret}"))
                .header(header::ACCEPT, "application/json, text/event-stream")
                .header(header::CONTENT_TYPE, "application/json")
                .header("Mcp-Protocol-Version", "2026-07-28")
                .header("Mcp-Method", "tools/list")
                .body(Body::from(
                    json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "method": "tools/list",
                        "params": {
                            "_meta": {
                                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                                "io.modelcontextprotocol/clientInfo": {
                                    "name": "test",
                                    "version": "1"
                                },
                                "io.modelcontextprotocol/clientCapabilities": {}
                            }
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let listed = body_sse_json(response).await;
    assert!(
        listed["result"]["ttlMs"].is_number(),
        "2026-07-28 tools/list must include a numeric ttlMs: {listed}"
    );
    assert!(
        matches!(
            listed["result"]["cacheScope"].as_str(),
            Some("public" | "private")
        ),
        "2026-07-28 tools/list must include a valid cacheScope: {listed}"
    );
}

#[tokio::test]
async fn modern_discovery_is_stateless() {
    let (state, _dir) = state().await;
    let secret = "tmg_modern";
    state
        .db
        .create_client_key("modern", "tmg_modern", &digest(secret), secret)
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
async fn admin_csrf_and_repeatable_client_keys() {
    let (state, _dir) = state().await;
    let gateway = app(state.clone());
    let login_body =
        json!({"username":"admin","password":"correct horse battery staple"}).to_string();
    let response = gateway
        .clone()
        .oneshot(
            Request::post("/api/login")
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
            assert!(value.contains("; Path=/;"));
            assert!(value.contains("SameSite=None"));
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
            Request::post("/api/providers")
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
            Request::post("/api/providers")
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
            Request::get("/api/providers")
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
    assert_eq!(listed[0]["connected"], false);
    assert_eq!(listed[0]["tool_mappings"], json!([]));

    let created_key_response = gateway
        .clone()
        .oneshot(
            Request::post("/api/keys")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::COOKIE, &cookie)
                .header("x-csrf-token", csrf)
                .body(Body::from(json!({"name":"repeatable"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(created_key_response.status(), StatusCode::CREATED);
    let created_key = body_json(created_key_response).await;
    let original_key = created_key["key"].as_str().unwrap().to_owned();
    let listed_keys = gateway
        .clone()
        .oneshot(
            Request::get("/api/keys")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let listed_keys = body_json(listed_keys).await;
    assert_eq!(listed_keys[0]["key"].as_str(), Some(original_key.as_str()));
}

#[tokio::test]
async fn admin_api_enforces_configured_cors_origins() {
    let (mut state, _dir) = state().await;
    state.admin.cors_origins = vec!["https://console.example".into()];
    let gateway = app(state);

    let preflight = gateway
        .clone()
        .oneshot(
            Request::builder()
                .method("OPTIONS")
                .uri("/api/login")
                .header("Origin", "https://console.example")
                .header("Access-Control-Request-Method", "POST")
                .header(
                    "Access-Control-Request-Headers",
                    "content-type,x-csrf-token",
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(preflight.status(), StatusCode::OK);
    assert_eq!(
        preflight
            .headers()
            .get("Access-Control-Allow-Origin")
            .unwrap(),
        "https://console.example"
    );
    assert_eq!(
        preflight
            .headers()
            .get("Access-Control-Allow-Credentials")
            .unwrap(),
        "true"
    );
    assert!(
        preflight
            .headers()
            .get("Access-Control-Allow-Headers")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("x-csrf-token")
    );

    let disallowed = gateway
        .clone()
        .oneshot(
            Request::get("/api/session")
                .header("Origin", "https://other.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(disallowed.status(), StatusCode::UNAUTHORIZED);
    assert!(
        !disallowed
            .headers()
            .contains_key("Access-Control-Allow-Origin")
    );

    let legacy = gateway
        .oneshot(Request::get("/admin").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(legacy.status(), StatusCode::NOT_FOUND);
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
        .create_client_key("usage", "tmg_usage", &digest(secret), secret)
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
async fn usage_overview_decodes_average_latency_from_sqlite() {
    let (state, _dir) = state().await;
    let client_key_id = state
        .db
        .create_client_key(
            "average",
            "tmg_average",
            &digest("average-secret"),
            "average-secret",
        )
        .await
        .unwrap();
    state
        .db
        .record_request(RequestRecord {
            id: "average-request",
            client_key_id,
            provider_id: None,
            duration_ms: 101,
            outcome: "success",
            error_category: None,
        })
        .await
        .unwrap();

    let overview = state
        .db
        .usage_overview()
        .await
        .expect("usage overview should decode SQLite AVG");
    assert_eq!(overview.average_latency_ms, 101);
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
            kind: ProviderKind::Searchix,
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
                kind: ProviderKind::Searchix,
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
async fn pending_research_is_polled_on_the_selected_provider_until_completed() {
    let mock = MockSearchix::default();
    mock.advertise_all_tools
        .store(true, std::sync::atomic::Ordering::SeqCst);
    mock.return_pending_research
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let research_api = MockResearchApi::default();
    research_api
        .pending_responses
        .store(1, std::sync::atomic::Ordering::SeqCst);
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
    let task = tokio::spawn({
        let research_api = research_api.clone();
        async move {
            let router = axum::Router::new()
                .nest_service("/mcp", service)
                .route(
                    "/api/tavily/research/{request_id}",
                    axum::routing::get(mock_research_status),
                )
                .with_state(research_api);
            let _ = axum::serve(listener, router).await;
        }
    });

    let (state, _dir) = state().await;
    state
        .providers
        .create(NewProvider {
            kind: ProviderKind::Searchix,
            name: "mock research".into(),
            endpoint: format!("http://{address}/mcp"),
            bearer_token: "test".into(),
            weight: 1,
            enabled: true,
        })
        .await
        .unwrap();
    let mut arguments = serde_json::Map::new();
    arguments.insert("provider_specific".into(), json!(true));
    let (_, result) = state
        .providers
        .call("tavily_research", arguments)
        .await
        .unwrap();
    let result = serde_json::to_value(result).unwrap();

    assert_eq!(result["isError"], false);
    assert_eq!(result["structuredContent"]["status"], "completed");
    assert_eq!(result["structuredContent"]["content"], "finished research");
    assert!(
        result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("finished research")
    );
    assert_eq!(
        research_api.calls.load(std::sync::atomic::Ordering::SeqCst),
        2
    );
    assert!(
        research_api
            .saw_bearer
            .load(std::sync::atomic::Ordering::SeqCst)
    );
    task.abort();
}

#[tokio::test]
async fn failed_provider_connections_abort_startup() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);

    let (state, _dir) = state().await;
    let unreachable = NewProvider {
        kind: ProviderKind::Searchix,
        name: "unreachable".into(),
        endpoint: format!("http://{address}/mcp"),
        bearer_token: "test".into(),
        weight: 1,
        enabled: true,
    };
    assert!(state.providers.create(unreachable.clone()).await.is_err());
    assert!(state.db.provider_summaries().await.unwrap().is_empty());

    state.db.create_provider(unreachable).await.unwrap();
    let startup = ProviderManager::new(state.db.clone()).await;
    let error = match startup {
        Ok(_) => panic!("an unavailable provider must prevent startup"),
        Err(error) => error,
    };
    assert!(
        error
            .to_string()
            .contains("failed to connect provider unreachable (1)")
    );
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
            Request::post("/api/login")
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
        .clone()
        .oneshot(
            Request::post("/api/providers")
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
    let listed = gateway
        .oneshot(
            Request::get("/api/providers")
                .header(header::COOKIE, cookies.join("; "))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let listed = body_json(listed).await;
    assert_eq!(listed[0]["connected"], true);
    assert_eq!(
        listed[0]["tool_mappings"],
        json!([{
            "canonical_tool": "tavily_search",
            "upstream_tool": "search_proxy_tavily_search"
        }])
    );
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
