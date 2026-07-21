-- AniList sync status (frontend AniList-sync feature): track the linked account's display
-- name and most recent sync time so the UI can show "Connected as X - last sync Ym ago"
-- without calling out to AniList on every page load.
ALTER TABLE external_accounts
  ADD COLUMN external_username text,
  ADD COLUMN last_synced_at timestamptz;
