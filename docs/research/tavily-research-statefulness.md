# `tavily_research` 是否无状态

Research date: 2026-08-30

## 结论

**如果“无状态”指跨两次 tool call 不保留研究上下文或对话记忆，那么 `tavily_research` 是无状态的。** 每次调用只接受完整的 `input` 和可选 `model`；schema 没有 conversation、thread、session、previous-result 或续写标识。官方 MCP handler 也只把本次调用的这两个字段交给 Research API，不会读取先前调用的报告。[^schema][^mcp-call]

但它**不是执行过程中完全没有状态**：非流式 Research API 会为每次调用创建一个独立的异步任务，返回唯一 `request_id`；随后用 `GET /research/{request_id}` 查询该任务的 `pending`、`in_progress`、`completed` 或 `failed` 状态。这个状态由 Tavily 服务端按 job 保存，而不是跨 tool call 的对话状态。[^create-api][^get-api]

官方 MCP 实现把异步过程封装成一个看似同步的 tool call：它在本次 handler 内保存 `requestId` 并轮询，直到返回最终报告或超时；`request_id` 不会作为 tool 结果暴露，也没有 resume/cancel/follow-up 参数。流式 fallback 同样只在本次调用内累积 `content`，完成后销毁流。[^mcp-poll][^mcp-stream]

## 轮询发生在哪一层

轮询发生在**上游官方 Tavily MCP 0.2.22 的 TypeScript `TavilyClient.research()` 方法**，不是本仓库的 Rust 网关，也不是调用网关的 MCP client。完整调用链是：

1. Rust 网关收到一次 `tools/call`，原样转发 arguments，然后只对一次 `provider.client.call_tool(params).await` 等待结果；网关代码没有 Tavily Research HTTP endpoint、`request_id` 状态判断或轮询循环。[^gateway-forward]
2. 官方 Tavily MCP handler 收到 `tavily_research`，用本次的 `input`、`model` 调用 `research()`。[^mcp-call]
3. `research()` 先调用 `POST https://api.tavily.com/research` 创建 job。0.2.22 实际发送的 JSON 是 `input`、`model`（缺省为 `auto`）以及非 keyless 模式下的 `api_key`；创建响应只读取 `request_id`。[^mcp-poll][^create-api]
4. MCP 进程在本地 sleep 后调用 `GET https://api.tavily.com/research/{request_id}`。它读取 `status`：`completed` 时读取并返回 `content`，`failed` 时返回 tool error；其他状态继续轮询，官方 REST schema 把这些非终态定义为 `pending`、`in_progress`。GET 返回 404 时 MCP 立即返回 `Research task not found`。[^mcp-poll][^get-api]

### 实际 HTTP contract

| 请求 | Tavily MCP 0.2.22 实际行为 | 官方 REST 响应字段 | MCP 实际消费字段 |
|---|---|---|---|
| `POST /research` | JSON body: `input`, `model`（缺省 `auto`）, `api_key`（有 key 时）；共享 axios client 同时携带 `Authorization: Bearer …`、`accept`、`content-type`、`X-Client-Source`、`X-Session-Id` | HTTP 201: `request_id`, `created_at`, `status`, `input`, `model`, `response_time` | 只取 `request_id` |
| `GET /research/{request_id}` | path 中放 POST 返回的 ID，无 request body，沿用共享 headers | HTTP 202: `request_id`, `status` (`pending`/`in_progress`), `response_time`; HTTP 200 completed: `request_id`, `created_at`, `status`, `content`, `sources`, `response_time`; HTTP 200 failed: `request_id`, `status`, `response_time` | 每次取 `status`；仅 completed 时再取 `content` |

请求 body、GET path 和被读取字段来自 0.2.22 的 `research()` 实现；共享 headers 来自同一版本的 axios client 初始化；完整 REST response schema 来自 Tavily 官方 Create/Get OpenAPI。[^mcp-poll][^official-session][^create-api][^get-api]

当前 REST API 的 POST schema 还支持 `stream`、`output_schema`、`citation_format`、`include_domains`、`exclude_domains`、`output_length`、`files`，但官方 MCP 0.2.22 的 tool schema 没有暴露这些参数，普通 polling POST 也不会发送它们。[^schema][^create-api] `sources` 同样不会被 0.2.22 的 MCP tool result 传回；该 tool 最终只格式化 `content`。[^mcp-call][^mcp-poll]

### 轮询参数

| 参数 | Tavily MCP 0.2.22 行为 |
|---|---|
| 首次等待 | 2 秒后发出第一次 GET |
| backoff | 每轮乘 1.5，最大 10 秒，即约 `2s → 3s → 4.5s → 6.75s → 10s → …` |
| `mini` 超时预算 | 5 分钟 |
| `pro` / `auto` / 未指定 model | 15 分钟；`auto` 按 pro 的上限处理 |
| 成功终态 | `completed`，返回 GET 响应的 `content`；0.2.22 不把 `request_id`、`sources` 等字段返回给 MCP 调用方 |
| 失败终态 | `failed` |
| 非终态 | 官方 API 定义为 `pending`、`in_progress`；实现实际上会对任何非 `completed`/`failed` status 继续等待 |

这里的“5/15 分钟”是源码用各次 sleep 时长累加的轮询预算，不包含 GET 自身耗时；循环会在 sleep 前检查预算，因此最后一次 sleep/GET 可能略微越过名义上限。[^mcp-poll]

还有一条不同的执行路径：如果首次非流式 POST 返回 HTTP 400 且 `detail.error_code == "research_stream_required"`，官方 MCP 会改为再次 `POST /research`，携带 `stream: true`，通过 SSE 等待 `done` 事件并在内存拼接 `choices[0].delta.content`；这条 fallback **不调用 GET、也不轮询 job status**。它的响应头等待上限为 30 秒、流 idle timeout 为 5 分钟、总时长上限仍为 mini 5 分钟/其他 15 分钟。[^mcp-stream]

## 状态边界

| 层级 | 是否有状态 | 生命周期与含义 |
|---|---|---|
| Research 语义 | 否（跨调用） | 每次输入是独立、完整的研究任务；没有历史消息或前序任务引用。 |
| Tavily 异步 job | 是 | 从 `POST /research` 创建到 `completed`/`failed`；官方 MCP 用本次任务唯一的 `request_id` 发 GET 查询。 |
| 单次上游 MCP tool call | 临时有状态 | 官方 Tavily MCP handler 暂存 `requestId`、轮询计时或流缓冲；调用结束即不再使用。 |
| 入站 MCP session | 有传输状态，但无研究记忆 | 本仓库为旧协议启用 `LocalSessionManager`，但每个 handler 只持有共享 `AppState`；`call_tool` 直接转交本次 arguments，没有按 session 保存研究内容。[^gateway-session][^gateway-call] |
| 上游连接 / 进程 | 有基础设施状态 | 网关长期复用已初始化的 provider client。官方 Tavily MCP 进程还会生成一次 `X-Session-Id` 并用于该进程发出的 HTTP 请求；源码和公开 schema 均未表明它会把先前研究内容作为后续调用上下文，因此只能把它认定为连接/归因标识，不能据此声称存在会话记忆。[^provider-client][^official-session] |
| 本仓库持久化 | 只存元数据 | 每次 call 生成网关自己的 UUID，并记录 client/provider、耗时、结果类别；测试明确检查数据库不含 query、arguments、results 或 content 列。因此本仓库不会持久化研究输入与报告用于下一次调用。[^usage-code][^usage-test] |

## 实际含义

- 想做追问时，调用方必须在新的 `input` 中重新带上必要上下文；不要期待同一个 MCP session 自动记住上一份报告。
- 同一个输入调用两次会发起两个独立 Research job；应按两次 API 调用对待，也不保证输出逐字相同。
- 如果网关、上游 MCP 进程或连接在 job 创建后、结果返回前中断，Tavily 端的 job 可能仍按其生命周期存在，但当前 `tavily_research` tool 没有向调用方暴露 `request_id`，所以不能通过该 tool 恢复轮询。
- 本结论对仓库中 pinned 的官方 Tavily MCP `0.2.22` 行为成立。网关会把 arguments 原样交给实际 provider；Searchix 的 `tavily_research` 实现尚无已验证的一手源码或 authenticated schema，因此不能把其内部隐藏状态等同于官方实现。[^passthrough][^provider-caveat]

## Primary sources

[^schema]: 本仓库 vendored 的官方 Tavily MCP 0.2.22 schema，`tavily_research` 只有 `input` 和 `model`，且只要求 `input`：[schemas/tavily-mcp-0.2.22.json](../../schemas/tavily-mcp-0.2.22.json)。对应官方源码 lines 423-441：<https://github.com/tavily-ai/tavily-mcp/blob/248dc9e3e385305ad3281120284ff662af4b5940/src/index.ts#L423-L441>
[^mcp-call]: 官方 MCP handler 从本次 arguments 构造 `{ input, model }` 并等待 `research()`，lines 446-449 与 528-538：<https://github.com/tavily-ai/tavily-mcp/blob/248dc9e3e385305ad3281120284ff662af4b5940/src/index.ts#L446-L449>、<https://github.com/tavily-ai/tavily-mcp/blob/248dc9e3e385305ad3281120284ff662af4b5940/src/index.ts#L528-L538>
[^create-api]: Tavily 官方 Create Research Task OpenAPI：`POST /research` 启动任务，非流式 `201` 返回唯一 `request_id` 与初始 status：<https://docs.tavily.com/documentation/api-reference/endpoint/research.md>
[^get-api]: Tavily 官方 Get Research Task Status OpenAPI：`GET /research/{request_id}` 按 ID 返回 pending/in_progress/completed/failed：<https://docs.tavily.com/documentation/api-reference/endpoint/research-get.md>
[^mcp-poll]: 官方 MCP `research()` 创建任务、把 `request_id` 保存在局部变量并轮询，lines 669-728：<https://github.com/tavily-ai/tavily-mcp/blob/248dc9e3e385305ad3281120284ff662af4b5940/src/index.ts#L669-L728>
[^mcp-stream]: 官方 MCP streaming fallback 的缓冲与释放都局限在一次 `researchViaStream()`，lines 746-876：<https://github.com/tavily-ai/tavily-mcp/blob/248dc9e3e385305ad3281120284ff662af4b5940/src/index.ts#L746-L876>
[^gateway-forward]: 本仓库的入站 handler 取出本次 arguments 后等待 provider 调用：[src/mcp.rs](../../src/mcp.rs#L59-L81)；provider 只构造一个上游 MCP `CallToolRequestParams` 并 `await` 一次 `client.call_tool`，没有 Tavily HTTP 请求或轮询：[src/provider.rs](../../src/provider.rs#L320-L347)。
[^gateway-session]: 本仓库用 `LocalSessionManager` 且对旧协议启用 session：[src/mcp.rs](../../src/mcp.rs#L85-L101)。
[^gateway-call]: 本仓库 handler 只取本次 `request.arguments` 并调用 provider：[src/mcp.rs](../../src/mcp.rs#L59-L81)。
[^provider-client]: 本仓库在 provider 激活时创建并保留一个 `RunningService`，后续调用复用它：[src/provider.rs](../../src/provider.rs#L505-L525)、[src/provider.rs](../../src/provider.rs#L322-L348)。
[^official-session]: 官方 Tavily MCP 在进程加载时生成一次 `SESSION_ID`，并配置为所有 HTTP 请求的 `X-Session-Id`，lines 15-18 与 98-108：<https://github.com/tavily-ai/tavily-mcp/blob/248dc9e3e385305ad3281120284ff662af4b5940/src/index.ts#L15-L18>、<https://github.com/tavily-ai/tavily-mcp/blob/248dc9e3e385305ad3281120284ff662af4b5940/src/index.ts#L98-L108>
[^usage-code]: 网关为每次调用生成日志 UUID，但只记录调用元数据：[src/provider.rs](../../src/provider.rs#L276-L319)、[src/provider.rs](../../src/provider.rs#L351-L382)。
[^usage-test]: 集成测试验证两次 call 是两个事件，且 `request_events` 没有 query、arguments、results 或 content 列：[tests/gateway.rs](../../tests/gateway.rs#L503-L649)。
[^passthrough]: 转发代码把本次 arguments 原样放入 `CallToolRequestParams`：[src/provider.rs](../../src/provider.rs#L322-L336)；mock 集成测试也断言 provider-specific arguments 未改变：[tests/gateway.rs](../../tests/gateway.rs#L85-L103)。
[^provider-caveat]: 已有的一手来源调查记录了 Searchix 的 Research 能力和 schema 尚未验证：[tavily-official-schema.md](tavily-official-schema.md#provider-specific-risk)。
