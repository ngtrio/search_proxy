CREATE TABLE admin_login_throttle (
  identity TEXT PRIMARY KEY,
  failed_attempts INTEGER NOT NULL DEFAULT 0,
  window_started_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  locked_until TEXT
);
INSERT INTO settings(key,value) VALUES ('default_timeout_seconds','120') ON CONFLICT(key) DO NOTHING;
INSERT INTO settings(key,value) VALUES ('default_cooldown_seconds','30') ON CONFLICT(key) DO NOTHING;
