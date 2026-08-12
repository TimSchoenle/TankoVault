CREATE INDEX IF NOT EXISTS chapters_source_disc_idx
    ON chapters (series_source_id, discovered_at DESC);

DROP INDEX IF EXISTS chapters_source_disc_access_idx;
