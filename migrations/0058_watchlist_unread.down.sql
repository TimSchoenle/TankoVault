DROP TRIGGER IF EXISTS watchlist_unread_early_access_delete ON user_provider_early_access;
DROP TRIGGER IF EXISTS watchlist_unread_early_access_insert ON user_provider_early_access;
DROP TRIGGER IF EXISTS watchlist_unread_watchlist_insert ON watchlist_entries;
DROP TRIGGER IF EXISTS watchlist_unread_progress_delete ON read_progress;
DROP TRIGGER IF EXISTS watchlist_unread_progress_update ON read_progress;
DROP TRIGGER IF EXISTS watchlist_unread_progress_insert ON read_progress;
DROP TRIGGER IF EXISTS watchlist_unread_sources_update ON series_sources;
DROP TRIGGER IF EXISTS watchlist_unread_sources_delete ON series_sources;
DROP TRIGGER IF EXISTS watchlist_unread_chapters_update ON chapters;
DROP TRIGGER IF EXISTS watchlist_unread_chapters_delete ON chapters;
DROP TRIGGER IF EXISTS watchlist_unread_chapters_insert ON chapters;

DROP FUNCTION IF EXISTS watchlist_unread_early_access_changed();
DROP FUNCTION IF EXISTS watchlist_unread_progress_moved();
DROP FUNCTION IF EXISTS watchlist_unread_progress_old();
DROP FUNCTION IF EXISTS watchlist_unread_progress_new();
DROP FUNCTION IF EXISTS watchlist_unread_refresh_keys(jsonb);
DROP FUNCTION IF EXISTS watchlist_unread_sources_updated();
DROP FUNCTION IF EXISTS watchlist_unread_sources_deleted();
DROP FUNCTION IF EXISTS watchlist_unread_chapters_updated();
DROP FUNCTION IF EXISTS watchlist_unread_chapters_deleted();
DROP FUNCTION IF EXISTS watchlist_unread_chapters_inserted();
DROP FUNCTION IF EXISTS refresh_watchlist_unread_for_series(uuid[]);
DROP FUNCTION IF EXISTS refresh_watchlist_unread(uuid, uuid[]);
DROP FUNCTION IF EXISTS watchlist_unread_live(uuid, uuid[]);

DROP TABLE IF EXISTS watchlist_unread;
