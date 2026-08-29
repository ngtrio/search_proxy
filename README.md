# Tavily MCP Gateway

A single-process Rust gateway exposing one authenticated `/mcp` endpoint and a stable `tavily_search` tool across Searchix and Tavily Hikari providers.

## Development

Requires Rust 1.88+ and pnpm.

```sh
cargo run
pnpm --dir web install
pnpm --dir web build
```

Set `DATABASE_URL`, `ADMIN_USERNAME`, and `ADMIN_PASSWORD`. Provider credentials are configured through the administration panel and are intentionally stored in plaintext SQLite fields; see [operations](docs/operations.md).

All enabled providers are connected before the gateway starts accepting traffic. Activation performs an authenticated initialize and a fully paginated `tools/list` lookup for the mapped search tool. Tool schemas and arguments are passed through without compatibility validation. Failed calls are never replayed to another provider.
