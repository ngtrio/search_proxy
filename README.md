# Tavily MCP Gateway

A single-process Rust gateway exposing one authenticated `/mcp` endpoint and the official Tavily MCP tool catalog across Searchix and Tavily Hikari providers:

- `tavily_search`
- `tavily_extract`
- `tavily_crawl`
- `tavily_map`
- `tavily_research`

The public catalog is pinned to Tavily MCP `0.2.22` at commit `248dc9e3e385305ad3281120284ff662af4b5940`; the vendored reference is [`schemas/tavily-mcp-0.2.22.json`](schemas/tavily-mcp-0.2.22.json).

## Architecture

The Rust service is the backend: it exposes the authenticated `/mcp` gateway, the liveness endpoint, and the administration API under `/api`. The administration console is a separate TanStack Start service, built with Vite and served in production by its Nitro-generated Node server; it is not embedded in the Rust binary. The console uses TanStack Router for URL-based page navigation and TanStack Query for cached API reads and mutation-driven invalidation.

The frontend sends credentialed requests to the backend configured by `VITE_API_BASE_URL`. Set `ADMIN_CORS_ORIGINS` to the exact frontend origin (including scheme and port). The administrator session uses Secure cross-site cookies and CSRF protection, so production frontend and backend endpoints must use HTTPS.

## Development

Requires Rust 1.88+ and pnpm. Start the backend with `ADMIN_CORS_ORIGINS` set to the frontend origin, then run the frontend separately:

```sh
pnpm --dir web install
ADMIN_CORS_ORIGINS=http://localhost:5173 ADMIN_COOKIE_SECURE=false cargo run
VITE_API_BASE_URL=http://localhost:3000 pnpm --dir web dev
```

Set `DATABASE_URL`, `ADMIN_USERNAME`, and `ADMIN_PASSWORD`. Provider credentials and client API keys are configured through the administration panel and are intentionally stored in plaintext SQLite fields; active client API keys are shown in the administration list for repeat copying; see [operations](docs/operations.md). `docker compose up -d --build` starts the backend on `127.0.0.1:3000` and the TanStack Start frontend on `127.0.0.1:8080`. For a production build outside Docker, run `VITE_API_BASE_URL=https://api.example.com pnpm --dir web build`, then `pnpm --dir web start`. Put your Caddy configuration in front of both services; if Caddy exposes `/api/*` on the same origin as the frontend, leave `VITE_API_BASE_URL` empty when building the frontend.

All enabled providers are connected during startup; failure to connect any enabled provider prevents the gateway from starting. Saving an enabled provider validates its connection before the configuration is persisted. Activation performs an authenticated initialize and a fully paginated `tools/list`, records the canonical Tavily tools that provider exposes, and requires at least one mapped tool. Calls are routed only to providers that advertised the requested tool. Tavily Hikari uses the canonical names; Searchix discovery recognizes both canonical names and its `search_proxy_` naming convention. Arguments and results are passed through without transformation. The rmcp client uses high-level `call_tool`; its default streamable HTTP transport may transparently reinitialize an expired upstream session and retry that ordinary request once. Other provider failures are returned as one generic provider error and are not routed to another provider.
