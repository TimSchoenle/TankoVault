-- Account settings (frontend redesign §9.4): per-user notification preferences.
--
-- Stored as an open JSONB document so new toggles (email digests, per-kind mutes,
-- quiet hours) can be added without a schema change. Defaults to an empty object,
-- meaning "use the product defaults"; the API layer supplies the effective values.
ALTER TABLE users
  ADD COLUMN notification_prefs jsonb NOT NULL DEFAULT '{}';
