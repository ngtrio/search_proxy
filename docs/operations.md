# Operations

The gateway is a single-process service. Its SQLite database contains plaintext upstream tokens by explicit design; protect the data volume and every backup as credentials.

## Deploy

Copy `.env.example` to `.env`, set a long administrator password, then run `docker compose up -d --build`. Put the service behind TLS. Restrict the Docker host, `/var/lib/docker/volumes`, and backup directory to administrators.

Liveness (`/health/live`) only confirms that the process responds.

An unavailable provider is logged and skipped during startup so the administration plane remains available for recovery.

## Reverse proxy

Nginx:

```nginx
location / {
    proxy_pass http://127.0.0.1:3000;
    proxy_http_version 1.1;
    proxy_buffering off;
    proxy_request_buffering off;
    proxy_read_timeout 180s;
    proxy_send_timeout 180s;
    proxy_set_header Host $host;
    proxy_set_header X-Forwarded-Proto https;
}
```

Caddy:

```caddy
search.example.com {
    reverse_proxy 127.0.0.1:3000 {
        flush_interval -1
        transport http { response_header_timeout 180s }
    }
}
```

## Backup and restore

Use SQLite's online backup command inside the volume so WAL contents are included:

```sh
sqlite3 /data/gateway.db ".backup '/data/backup-$(date +%F).db'"
chmod 600 /data/backup-*.db
sqlite3 /data/backup-2026-08-27.db 'PRAGMA integrity_check;'
```

Copy the backup to access-controlled encrypted storage. A backup contains plaintext provider tokens.

To exercise restore, stop the gateway, preserve the current database files, copy a verified backup to a new `gateway.db`, set ownership/permissions, and start the gateway. Confirm migrations, login, key metadata, provider records, and liveness. Do not restore over a running WAL database.

## Rollback

Before an upgrade, take and verify an online backup. Keep the previous immutable image. If rollback is needed, stop the new container, restore the pre-upgrade database when migrations are not backward-compatible, select the prior image, and start it. Confirm liveness before returning traffic.

Structured logs include request identifiers, provider/client metadata IDs, durations, and outcome categories only. They must never include search queries, arguments, results, passwords, client keys, or provider tokens.
