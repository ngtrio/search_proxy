# Tavily MCP Gateway

A single-process Rust gateway exposing one authenticated `/mcp` endpoint and the official Tavily MCP tool catalog across Searchix and Tavily Hikari providers:

- `tavily_search`
- `tavily_extract`
- `tavily_crawl`
- `tavily_map`
- `tavily_research`

The public catalog is pinned to Tavily MCP `0.2.22` at commit `248dc9e3e385305ad3281120284ff662af4b5940`; the vendored reference is [`schemas/tavily-mcp-0.2.22.json`](schemas/tavily-mcp-0.2.22.json).

## Development

Requires Rust 1.88+ and pnpm.

```sh
cargo run
pnpm --dir web install
pnpm --dir web build
```

Set `DATABASE_URL`, `ADMIN_USERNAME`, and `ADMIN_PASSWORD`. Provider credentials and client API keys are configured through the administration panel and are intentionally stored in plaintext SQLite fields; active client API keys are shown in the administration list for repeat copying; see [operations](docs/operations.md). Serve the administration plane through HTTPS; its session cookies are `Secure`.

All enabled providers are connected during startup; failure to connect any enabled provider prevents the gateway from starting. Saving an enabled provider validates its connection before the configuration is persisted. Activation performs an authenticated initialize and a fully paginated `tools/list`, records the canonical Tavily tools that provider exposes, and requires at least one mapped tool. Calls are routed only to providers that advertised the requested tool. Tavily Hikari uses the canonical names; Searchix discovery recognizes both canonical names and its `search_proxy_` naming convention. Arguments and results are passed through without transformation. The rmcp client uses high-level `call_tool`; its default streamable HTTP transport may transparently reinitialize an expired upstream session and retry that ordinary request once. Other provider failures are returned as one generic provider error and are not routed to another provider.
