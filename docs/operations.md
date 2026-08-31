# Operations

The gateway backend is a single-process service, deployed alongside a separate TanStack Start administration frontend. The SQLite database contains plaintext upstream tokens and client API keys by explicit design; active client API keys are also visible to authenticated administrators in the key list. Protect the data volume and every backup as credentials.

## Deploy

Copy `.env.example` to `.env`, set a long administrator password, set `ADMIN_CORS_ORIGINS` to the public frontend origin, and add the public gateway hostname to `MCP_ALLOWED_HOSTS`. `docker compose up -d --build` builds and starts both the Rust gateway and the TanStack Start frontend. The frontend listens on `127.0.0.1:3001`; the backend listens on `127.0.0.1:3000`. Put your Caddy deployment in front of them: route `/api/*` and `/mcp*` to the gateway and all other paths to the frontend. Set `VITE_API_BASE_URL` to the public backend URL before building, or leave it empty when Caddy exposes `/api` on the frontend's same public origin. Put both services behind TLS and set `ADMIN_COOKIE_SECURE=true`. Restrict the Docker host, the repository's `data` directory, and the backup directory to administrators.

Compose bind-mounts the repository's `./data` directory at `/data` in the gateway container. Create it before the first start and ensure it is writable by the container's UID 10001. The directory is excluded from Git because it contains the live SQLite database and plaintext credentials.

The Compose deployment publishes the backend on `127.0.0.1:3000` and the frontend on `127.0.0.1:3001`. The backend does not serve the frontend or `/admin` routes. Administration endpoints are available under `/api`; the frontend is a Node service, not a static `web/dist` directory.

Liveness (`/health/live`) only confirms that the process responds.

An enabled provider that cannot connect during startup causes the gateway to fail to start. Restart it after the provider is available again.

## TLS and request handling

This repository does not install or configure a reverse proxy. Terminate TLS in the deployment environment and keep that configuration with the infrastructure that owns it. Configure `ADMIN_CORS_ORIGINS` as an exact allowlist; wildcard origins are rejected. The browser must be able to reach the public value of `VITE_API_BASE_URL`.

MCP Streamable HTTP responses may use SSE, and long-running tools may keep one request open for several minutes. Every hop in front of the gateway must therefore pass streaming responses without buffering and use request, response-header, and idle timeouts suitable for the longest enabled provider call. The gateway does not impose a per-provider tool-call timeout.

The Compose healthcheck's three-second timeout applies only to `/health/live`; it is not a limit on MCP tool calls.

## Backup and restore

Use SQLite's online backup command against the mounted `/data` directory so WAL contents are included:

```sh
sqlite3 /data/gateway.db ".backup '/data/backup-$(date +%F).db'"
chmod 600 /data/backup-*.db
sqlite3 /data/backup-2026-08-27.db 'PRAGMA integrity_check;'
```

Copy the backup to access-controlled encrypted storage. A backup contains plaintext provider tokens and client API keys.

To exercise restore, stop the gateway, preserve the current database files, copy a verified backup to a new `gateway.db`, set ownership/permissions, and start the gateway. Confirm migrations, login, key metadata, provider records, and liveness. Do not restore over a running WAL database.

## Rollback

Before an upgrade, take and verify an online backup. Keep the previous immutable image. If rollback is needed, stop the new container, restore the pre-upgrade database when migrations are not backward-compatible, select the prior image, and start it. Confirm liveness before returning traffic.

Structured logs include request identifiers, provider/client metadata IDs, durations, and outcome categories only. They must never include search queries, arguments, results, passwords, client keys, or provider tokens.
