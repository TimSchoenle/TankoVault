-- Restore the two-column index first, so no window leaves the unread predicate with nothing
-- to lean on at all.
CREATE INDEX IF NOT EXISTS chapters_source_floor_idx
    ON chapters (series_source_id, (floor(number)));

DROP INDEX IF EXISTS chapters_source_floor_num_idx;
