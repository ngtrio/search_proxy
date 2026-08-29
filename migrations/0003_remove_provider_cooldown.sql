ALTER TABLE providers DROP COLUMN base_cooldown_seconds;
ALTER TABLE provider_probe_results DROP COLUMN cooldown_until;
ALTER TABLE provider_probe_results DROP COLUMN failure_count;
UPDATE provider_probe_results SET health = 'incompatible' WHERE health = 'cooldown';
DELETE FROM settings WHERE key = 'default_cooldown_seconds';
