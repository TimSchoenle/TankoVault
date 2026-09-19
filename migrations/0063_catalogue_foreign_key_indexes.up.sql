-- Index every column that references `series` or `series_sources`.
--
-- An `ON DELETE` action runs once per deleted parent row as `… WHERE fk_column = $1`. These
-- twelve columns had no index leading on them, so deleting one series sequentially scanned each
-- of their tables. A 500-series purge batch took 30 s on production, hit the request timeout and
-- rolled back, so "Wipe the entire catalogue" could never finish. Pinned by
-- `crates/db/tests/schema_foreign_key_indexes.rs`.
--
-- Not `CREATE INDEX CONCURRENTLY`, for the reason `0020_performance_indexes` gives. On a large
-- database, run each statement below by hand with `CONCURRENTLY` first; the migration then finds
-- the work done.

CREATE INDEX IF NOT EXISTS watchlist_entries_series_idx ON watchlist_entries (series_id);
CREATE INDEX IF NOT EXISTS watchlist_entries_pinned_source_idx ON watchlist_entries (pinned_source_id)
    WHERE pinned_source_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS read_progress_series_idx ON read_progress (series_id);
CREATE INDEX IF NOT EXISTS notification_dedup_series_idx ON notification_dedup (series_id);
CREATE INDEX IF NOT EXISTS merge_candidates_candidate_idx ON merge_candidates (candidate_id);
CREATE INDEX IF NOT EXISTS sync_remote_entries_series_idx ON sync_remote_entries (series_id)
    WHERE series_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS series_sync_overrides_series_idx ON series_sync_overrides (series_id);
CREATE INDEX IF NOT EXISTS sync_conflicts_series_idx ON sync_conflicts (series_id);
CREATE INDEX IF NOT EXISTS sync_history_series_idx ON sync_history (series_id);
CREATE INDEX IF NOT EXISTS series_cooccurrence_other_idx ON series_cooccurrence (other_id);
CREATE INDEX IF NOT EXISTS user_series_affinity_series_idx ON user_series_affinity (series_id);
CREATE INDEX IF NOT EXISTS recommendation_feedback_series_idx ON recommendation_feedback (series_id);
