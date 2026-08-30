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

Set `DATABASE_URL`, `ADMIN_USERNAME`, and `ADMIN_PASSWORD`. Provider credentials are configured through the administration panel and are intentionally stored in plaintext SQLite fields; see [operations](docs/operations.md).

Enabled providers are connected during startup, but one unavailable provider does not prevent the administration plane from starting. Readiness remains false until the database is healthy and at least one provider is connected. Saving an enabled provider validates its connection before the configuration is persisted. Activation performs an authenticated initialize and a fully paginated `tools/list`, records the canonical Tavily tools that provider exposes, and requires at least one mapped tool. Calls are routed only to providers that advertised the requested tool. Tavily Hikari uses the canonical names; Searchix discovery recognizes both canonical names and its `search_proxy_` naming convention. Arguments and results are passed through without transformation, and failed calls are never replayed to another provider. A failed transport is reconnected before a later call instead of replaying the failed call.
