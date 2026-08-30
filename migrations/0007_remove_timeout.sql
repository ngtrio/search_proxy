ALTER TABLE providers DROP COLUMN timeout_seconds;
DELETE FROM settings WHERE key = 'default_timeout_seconds';
