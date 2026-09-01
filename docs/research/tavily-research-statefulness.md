# `tavily_research` 是否无状态

Research date: 2026-08-30; provider verification: 2026-09-01

## 结论

**如果“无状态”指跨两次 tool call 不保留研究上下文或对话记忆，那么 `tavily_research` 是无状态的。** 每次调用只接受完整的 `input` 和可选 `model`；schema 没有 conversation、thread、session、previous-result 或续写标识。官方 MCP handler 也只把本次调用的这两个字段交给 Research API，不会读取先前调用的报告。[^schema][^mcp-call]

但它**不是执行过程中完全没有状态**：非流式 Research API 会为每次调用创建一个独立的异步任务，返回唯一 `request_id`；随后用 `GET /research/{request_id}` 查询该任务的 `pending`、`in_progress`、`completed` 或 `failed` 状态。这个状态由 Tavily 服务端按 job 保存，而不是跨 tool call 的对话状态。[^create-api][^get-api]

官方 MCP 实现把异步过程封装成一个看似同步的 tool call：它在本次 handler 内保存 `requestId` 并轮询，直到返回最终报告或超时；`request_id` 不会作为 tool 结果暴露，也没有 resume/cancel/follow-up 参数。流式 fallback 同样只在本次调用内累积 `content`，完成后销毁流。[^mcp-poll][^mcp-stream]

## 轮询发生在哪一层

官方正常路径的轮询发生在**上游官方 Tavily MCP 0.2.22 的 TypeScript `TavilyClient.research()` 方法**，不是调用网关的 MCP client。完整调用链是：

1. Rust 网关收到一次 `tools/call`，原样转发 arguments，并等待一次 `provider.client.call_tool(params).await`。[^gateway-forward]
2. 官方 Tavily MCP handler 收到 `tavily_research`，用本次的 `input`、`model` 调用 `research()`。[^mcp-call]
3. `research()` 先调用 `POST https://api.tavily.com/research` 创建 job。0.2.22 实际发送的 JSON 是 `input`、`model`（缺省为 `auto`）以及非 keyless 模式下的 `api_key`；创建响应只读取 `request_id`。[^mcp-poll][^create-api]
4. MCP 进程在本地 sleep 后调用 `GET https://api.tavily.com/research/{request_id}`。它读取 `status`：`completed` 时读取并返回 `content`，`failed` 时返回 tool error；其他状态继续轮询，官方 REST schema 把这些非终态定义为 `pending`、`in_progress`。GET 返回 404 时 MCP 立即返回 `Research task not found`。[^mcp-poll][^get-api]

本仓库还为行为不同的 provider 实现了兼容 fallback：如果上游 MCP 把含 `request_id` 的 `pending` / `in_progress` REST JSON 直接作为 tool result 返回，网关不会结束入站调用，而是固定使用刚才选中的 provider，以同一个 bearer token 查询该 origin 的 `/api/tavily/research/{request_id}`。它按 `2s → 3s → 4.5s → … → 10s` 退避，`mini` 最多五分钟、其他 model 最多十五分钟；`completed` 的完整 JSON 同时作为 MCP text 和 `structuredContent` 返回，`failed` 作为 tool error 返回。[^gateway-forward]

### Claude 等 MCP client 怎么拿结果

按 Tavily 发布的官方用法，Claude Desktop、Claude Code 或其他 MCP client 只需连接 Tavily MCP endpoint，然后调用一次 `tavily_research`；Tavily 文档给出的 Claude 配置也是把客户端直接接到 `https://mcp.tavily.com/mcp/`，没有要求客户端自行实现 Research REST 轮询。[^mcp-docs] 在仓库锁定的官方 MCP 实现中，`tools/call` handler 会 `await this.research(...)`，而 `research()` 自己保存创建响应中的 `request_id` 并反复调用 GET。因此，对 Claude 来说这是一个耗时较长、最终返回报告的**单次 MCP tool call**，不是“先得到 pending，再由模型发第二次 MCP 调用”。[^mcp-call][^mcp-poll]

如果 MCP tool result 已经把下面这类 REST 创建响应作为最终文本返回给 Claude：

```json
{"request_id":"…","status":"pending"}
```

那么该次 MCP 调用在客户端看来已经结束。Claude 不会仅凭 JSON 中出现 `request_id` 就绕过 MCP server、自动请求 Tavily REST API。MCP 2025-11-25 虽然另有实验性的 Tasks 协议，但它要求 server 在初始化时声明 `tasks.requests.tools.call`、tool 声明 `execution.taskSupport`，并返回带 `task.taskId` 的 `CreateTaskResult`；调用方随后用 `tasks/get` 和 `tasks/result`，而不是识别 tool 文本中的任意 `request_id`。[^mcp-tasks] Tavily MCP 0.2.22 只声明 `tools` capability，`tavily_research` 也没有 `execution.taskSupport`，所以上面的 Tavily REST JSON 不是 MCP Task。当前 Tavily tool schema 只有 `input` 和 `model`，catalog 中也没有 `get_research` / `research_status` 工具，因此 Claude 无法通过这套 MCP catalog 查询已有 job；再次调用 `tavily_research` 只会提交一个新任务。[^schema][^mcp-call]

直接使用 Tavily REST/SDK 是另一套 contract：非流式 `research()`/`POST /research` 返回 `request_id` 和 `pending` 后，应用程序要调用 `get_research(request_id)`/`GET /research/{request_id}`，直到 `completed` 或 `failed`；官方 Get Research 文档分别给出了 Python `get_research(...)` 和 JavaScript `get_research(...)` 示例。也可选择 `stream: true` 的 SSE 路径等待流结束。[^create-api][^get-api]

所以，经 MCP 返回裸 `pending` 不是 Claude 少做了一步，而是提供该 tool 的 MCP server/代理没有完成官方实现原本封装的等待流程，或者部署的行为与这里验证的官方版本不一致。本仓库现在会兼容这种响应并替 Claude 轮询；通用 MCP client 本身仍不会自动补齐 Tavily 私有 REST 流程。

### 当前两个 provider 的查询能力（2026-09-01 实测）

两个已配置 provider 的 MCP `tools/list` 都只公开 Research **提交工具**，没有 `research_status` / `get_research` 工具：Hikari 是 `tavily_research`，Searchix 是 `search_proxy_tavily_research`。因此 Agent 不能通过当前 MCP catalog 查询已有任务；网关的兼容轮询发生在同一次 `tavily_research` 调用内部。[^provider-live-tools]

但两者都提供 MCP 之外的 Tavily-compatible HTTP 查询路由，认证使用各 provider 自己的 bearer token：

| Provider | 查询接口 | 对给定 `request_id` 的只读实测 |
|---|---|---|
| Tavily Hikari | `GET https://tavily.ivanli.cc/api/tavily/research/{request_id}` | 路由存在；对该 ID 返回 HTTP 404 `research_request_not_found` |
| Searchix | `GET https://search.604020.xyz/api/tavily/research/{request_id}` | 返回 HTTP 200、`status: completed`、完整 `content` 与 `sources` |

Hikari 官方文档明确把 `GET /api/tavily/research/:request_id` 列为对 Tavily `GET /research/{request_id}` 的代理，并要求 Hikari access token。[^hikari-http-research] Searchix 未发现公开 OpenAPI 或源码文档；其支持结论来自当前部署的 authenticated read-only 响应，而不是从命名推断。[^provider-live-http]

同一 ID 在 Searchix 成功、在 Hikari 不存在，说明 `request_id` 不能跨 provider 查询。网关的兼容实现因此持有本次提交实际选择的 provider，并使用该 provider 的 HTTP origin 与 bearer token；轮询不会重新负载均衡。

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
| 网关兼容轮询 | 临时有状态 | 仅当上游提前返回 `pending` / `in_progress` 时，网关在本次入站 tool call 内暂存 `request_id` 与选中的 provider，直到终态或超时；不会持久化 job。 |
| 入站 MCP session | 有传输状态，但无研究记忆 | 本仓库为旧协议启用 `LocalSessionManager`，但每个 handler 只持有共享 `AppState`；`call_tool` 直接转交本次 arguments，没有按 session 保存研究内容。[^gateway-session][^gateway-call] |
| 上游连接 / 进程 | 有基础设施状态 | 网关长期复用已初始化的 provider client。官方 Tavily MCP 进程还会生成一次 `X-Session-Id` 并用于该进程发出的 HTTP 请求；源码和公开 schema 均未表明它会把先前研究内容作为后续调用上下文，因此只能把它认定为连接/归因标识，不能据此声称存在会话记忆。[^provider-client][^official-session] |
| 本仓库持久化 | 只存元数据 | 每次 call 生成网关自己的 UUID，并记录 client/provider、耗时、结果类别；测试明确检查数据库不含 query、arguments、results 或 content 列。因此本仓库不会持久化研究输入与报告用于下一次调用。[^usage-code][^usage-test] |

## 实际含义

- 想做追问时，调用方必须在新的 `input` 中重新带上必要上下文；不要期待同一个 MCP session 自动记住上一份报告。
- 同一个输入调用两次会发起两个独立 Research job；应按两次 API 调用对待，也不保证输出逐字相同。
- 如果网关、上游 MCP 进程或连接在 job 创建后、结果返回前中断，Tavily 端的 job 可能仍按其生命周期存在，但当前 `tavily_research` tool 没有向调用方暴露 `request_id`，所以不能通过该 tool 恢复轮询。
- 本结论对仓库中 pinned 的官方 Tavily MCP `0.2.22` 行为成立。网关会把 arguments 原样交给实际 provider；Searchix 当前部署已验证会公开 Research 提交工具并提供 HTTP 状态查询，但其内部轮询/状态保存实现仍无公开源码，因此不能把其内部机制等同于官方 MCP 实现。[^passthrough][^provider-live-tools][^provider-live-http]

## Primary sources

[^schema]: 本仓库 vendored 的官方 Tavily MCP 0.2.22 schema，`tavily_research` 只有 `input` 和 `model`，且只要求 `input`：[schemas/tavily-mcp-0.2.22.json](../../schemas/tavily-mcp-0.2.22.json)。对应官方源码 lines 423-441：<https://github.com/tavily-ai/tavily-mcp/blob/248dc9e3e385305ad3281120284ff662af4b5940/src/index.ts#L423-L441>
[^mcp-call]: 官方 MCP handler 从本次 arguments 构造 `{ input, model }` 并等待 `research()`，lines 446-449 与 528-538：<https://github.com/tavily-ai/tavily-mcp/blob/248dc9e3e385305ad3281120284ff662af4b5940/src/index.ts#L446-L449>、<https://github.com/tavily-ai/tavily-mcp/blob/248dc9e3e385305ad3281120284ff662af4b5940/src/index.ts#L528-L538>
[^create-api]: Tavily 官方 Create Research Task OpenAPI：`POST /research` 启动任务，非流式 `201` 返回唯一 `request_id` 与初始 status：<https://docs.tavily.com/documentation/api-reference/endpoint/research.md>
[^get-api]: Tavily 官方 Get Research Task Status OpenAPI：`GET /research/{request_id}` 按 ID 返回 pending/in_progress/completed/failed：<https://docs.tavily.com/documentation/api-reference/endpoint/research-get.md>
[^mcp-poll]: 官方 MCP `research()` 创建任务、把 `request_id` 保存在局部变量并轮询，lines 669-728：<https://github.com/tavily-ai/tavily-mcp/blob/248dc9e3e385305ad3281120284ff662af4b5940/src/index.ts#L669-L728>
[^mcp-stream]: 官方 MCP streaming fallback 的缓冲与释放都局限在一次 `researchViaStream()`，lines 746-876：<https://github.com/tavily-ai/tavily-mcp/blob/248dc9e3e385305ad3281120284ff662af4b5940/src/index.ts#L746-L876>
[^mcp-docs]: Tavily 官方 MCP 文档列出 remote endpoint，并分别给出 Claude Desktop integration 与 Claude Code remote MCP/OAuth 配置；客户端配置中没有 Research status endpoint 或轮询步骤：<https://docs.tavily.com/documentation/mcp.md>
[^mcp-tasks]: MCP 2025-11-25 的实验性 Tasks 规范说明 capability/tool-level negotiation、`CreateTaskResult`、`tasks/get` 与 `tasks/result`：<https://modelcontextprotocol.io/specification/2025-11-25/basic/utilities/tasks.md>
[^gateway-forward]: 本仓库的入站 handler 取出本次 arguments 后等待 provider 调用：[src/mcp.rs](../../src/mcp.rs#L59-L81)；provider 构造上游 MCP `CallToolRequestParams`，并在 Research 返回非终态时固定原 provider 执行 HTTP 轮询：[src/provider.rs](../../src/provider.rs)。
[^gateway-session]: 本仓库用 `LocalSessionManager` 且对旧协议启用 session：[src/mcp.rs](../../src/mcp.rs#L85-L101)。
[^gateway-call]: 本仓库 handler 只取本次 `request.arguments` 并调用 provider：[src/mcp.rs](../../src/mcp.rs#L59-L81)。
[^provider-client]: 本仓库在 provider 激活时创建并保留一个 `RunningService`，后续调用复用它：[src/provider.rs](../../src/provider.rs#L505-L525)、[src/provider.rs](../../src/provider.rs#L322-L348)。
[^official-session]: 官方 Tavily MCP 在进程加载时生成一次 `SESSION_ID`，并配置为所有 HTTP 请求的 `X-Session-Id`，lines 15-18 与 98-108：<https://github.com/tavily-ai/tavily-mcp/blob/248dc9e3e385305ad3281120284ff662af4b5940/src/index.ts#L15-L18>、<https://github.com/tavily-ai/tavily-mcp/blob/248dc9e3e385305ad3281120284ff662af4b5940/src/index.ts#L98-L108>
[^usage-code]: 网关为每次调用生成日志 UUID，但只记录调用元数据：[src/provider.rs](../../src/provider.rs#L276-L319)、[src/provider.rs](../../src/provider.rs#L351-L382)。
[^usage-test]: 集成测试验证两次 call 是两个事件，且 `request_events` 没有 query、arguments、results 或 content 列：[tests/gateway.rs](../../tests/gateway.rs#L503-L649)。
[^passthrough]: 转发代码把本次 arguments 原样放入 `CallToolRequestParams`：[src/provider.rs](../../src/provider.rs#L322-L336)；mock 集成测试也断言 provider-specific arguments 未改变：[tests/gateway.rs](../../tests/gateway.rs#L85-L103)。
[^hikari-http-research]: Tavily Hikari 官方 README 把 `/api/tavily` 定义为 Tavily HTTP façade，并说明会在 `TAVILY_USAGE_BASE` 下使用 `/research/{id}`：<https://github.com/IvanLi-CN/tavily-hikari/blob/c65f8a405d18ad1e2f53356098eae16a6597da13/README.md>。其官方 HTTP proxy 文档明确列出 `GET /api/tavily/research/:request_id`、对应 Tavily `GET /research/{request_id}`，并规定 `Authorization: Bearer th-<id>-<secret>`：<https://github.com/IvanLi-CN/tavily-hikari/blob/c65f8a405d18ad1e2f53356098eae16a6597da13/docs/tavily-http-api-proxy.md>。
[^provider-live-tools]: 2026-09-01 使用仓库中各 provider 已配置的 bearer token 对 `https://tavily.ivanli.cc/mcp` 与 `https://search.604020.xyz/mcp` 执行只读 MCP `tools/list`；两份响应均有 Research 提交工具，均无名称含 `research_status` 或 `get_research` 的工具。未调用任何 billable tool。
[^provider-live-http]: 2026-09-01 使用用户给出的 `request_id` 和各 provider 已配置的 bearer token 执行只读 GET。Searchix 的 `/api/tavily/research/{request_id}` 返回 HTTP 200、`status: completed`；Hikari 的同形路由返回 HTTP 404 JSON `research_request_not_found`。没有提交新 Research 任务，也没有在记录中保存或输出 bearer token。
