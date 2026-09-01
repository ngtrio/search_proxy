# Tavily MCP Gateway

A single-process Rust gateway exposing one authenticated `/mcp` endpoint and the official Tavily MCP tool catalog across Searchix and Tavily Hikari providers:

- `tavily_search`
- `tavily_extract`
- `tavily_crawl`
- `tavily_map`
- `tavily_research`

The public catalog is pinned to Tavily MCP `0.2.22` at commit `248dc9e3e385305ad3281120284ff662af4b5940`; the vendored reference is [`schemas/tavily-mcp-0.2.22.json`](schemas/tavily-mcp-0.2.22.json).

## Architecture

The Rust service is the backend: it exposes the authenticated `/mcp` gateway, the liveness endpoint, the authenticated administration API, and a public read-only `GET /api/metrics` endpoint. The separate TanStack Start frontend is a single-page traffic monitor built with Vite and served in production by its Nitro-generated Node server; it is not embedded in the Rust binary. TanStack Query caches each selected time window and refreshes the aggregate metrics once per minute.

The frontend reads aggregate metrics from the backend configured by `VITE_API_BASE_URL`. Set `ADMIN_CORS_ORIGINS` to the exact frontend origin (including scheme and port) when the services use different origins. Existing administrator routes remain authenticated and continue to use Secure cookies and CSRF protection.

The MCP transport validates the inbound `Host` header to prevent DNS rebinding. Public deployments must add their exact gateway hostname to the comma-separated `MCP_ALLOWED_HOSTS` list, for example `MCP_ALLOWED_HOSTS=localhost,127.0.0.1,mcp.example.com`. A hostname entry allows that hostname on any port; use `hostname:port` to restrict it to one port.

`GET /api/metrics?window=1h|24h|7d|30d` returns request count, success rate, P50/P95 latency, the preceding-period comparison, a zero-filled traffic series, 24 hours of five-minute activity buckets, and 24 hourly latency distributions. Timestamps are UTC and the monitor displays them as Asia/Shanghai (UTC+08:00). The response contains aggregates only and never includes request IDs, client keys, providers, or error details.

## Development

Requires Rust 1.88+ and pnpm. Start the backend with `ADMIN_CORS_ORIGINS` set to the frontend origin, then run the frontend separately:

```sh
pnpm --dir web install
ADMIN_CORS_ORIGINS=http://localhost:5173 ADMIN_COOKIE_SECURE=false cargo run
VITE_API_BASE_URL=http://localhost:3000 pnpm --dir web dev
```

Set `DATABASE_URL`, `ADMIN_USERNAME`, and `ADMIN_PASSWORD`. Provider credentials and client API keys remain available through the authenticated administration API; see [operations](docs/operations.md). `docker compose up -d --build` starts the backend on `127.0.0.1:3000` and the TanStack Start monitor on `127.0.0.1:3001`. For a production build outside Docker, run `VITE_API_BASE_URL=https://api.example.com pnpm --dir web build`, then `pnpm --dir web start`. Put your Caddy configuration in front of both services; if Caddy exposes `/api/*` on the same origin as the frontend, leave `VITE_API_BASE_URL` empty when building the frontend.

All enabled providers are connected during startup; failure to connect any enabled provider prevents the gateway from starting. Saving an enabled provider validates its connection before the configuration is persisted. Activation performs an authenticated initialize and a fully paginated `tools/list`, records the canonical Tavily tools that provider exposes, and requires at least one mapped tool. Calls are routed only to providers that advertised the requested tool. Tavily Hikari uses the canonical names; Searchix discovery recognizes both canonical names and its `search_proxy_` naming convention. Arguments and ordinary results are passed through without transformation. If `tavily_research` instead returns a Tavily REST `pending`/`in_progress` response, the gateway keeps the selected provider pinned and polls that provider's same-origin `/api/tavily/research/{request_id}` endpoint with its bearer token. It returns the terminal JSON as both text and structured content; `mini` polls for at most five minutes and other models for at most fifteen minutes. The rmcp client uses high-level `call_tool`; its default streamable HTTP transport may transparently reinitialize an expired upstream session and retry that ordinary request once. Other provider failures are returned as one generic provider error and are not routed to another provider.
