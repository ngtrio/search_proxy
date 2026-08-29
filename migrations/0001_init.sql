CREATE TABLE admins (
  id INTEGER PRIMARY KEY CHECK (id = 1), username TEXT NOT NULL UNIQUE,
  password_hash TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE TABLE admin_sessions (
  id TEXT PRIMARY KEY, token_digest BLOB NOT NULL UNIQUE, csrf_digest BLOB NOT NULL,
  expires_at TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE TABLE client_api_keys (
  id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, prefix TEXT NOT NULL,
  digest BLOB NOT NULL UNIQUE, status TEXT NOT NULL DEFAULT 'active',
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, last_used_at TEXT, request_count INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE providers (
  id INTEGER PRIMARY KEY AUTOINCREMENT, kind TEXT NOT NULL CHECK(kind IN ('searchix','tavily_hikari')),
  name TEXT NOT NULL, endpoint TEXT NOT NULL, bearer_token TEXT NOT NULL,
  weight INTEGER NOT NULL DEFAULT 1 CHECK(weight > 0), enabled INTEGER NOT NULL DEFAULT 1,
  timeout_seconds INTEGER NOT NULL DEFAULT 120 CHECK(timeout_seconds > 0),
  base_cooldown_seconds INTEGER NOT NULL DEFAULT 30 CHECK(base_cooldown_seconds > 0),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE TABLE provider_probe_results (
  provider_id INTEGER PRIMARY KEY REFERENCES providers(id) ON DELETE CASCADE,
  compatible INTEGER NOT NULL DEFAULT 0, health TEXT NOT NULL, protocol_version TEXT,
  schema_fingerprint TEXT, error_category TEXT, cooldown_until TEXT, failure_count INTEGER NOT NULL DEFAULT 0,
  probed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE TABLE request_events (
  id TEXT PRIMARY KEY, client_key_id INTEGER REFERENCES client_api_keys(id), provider_id INTEGER REFERENCES providers(id),
  started_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, duration_ms INTEGER NOT NULL,
  outcome TEXT NOT NULL, error_category TEXT
);
CREATE INDEX request_events_started_at ON request_events(started_at);
CREATE TABLE usage_daily (
  day TEXT NOT NULL, client_key_id INTEGER, provider_id INTEGER, outcome TEXT NOT NULL,
  requests INTEGER NOT NULL, duration_ms INTEGER NOT NULL,
  PRIMARY KEY(day,client_key_id,provider_id,outcome)
);
CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
INSERT INTO settings(key,value) VALUES ('retention_days','30');

