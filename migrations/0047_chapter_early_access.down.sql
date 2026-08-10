DROP TABLE IF EXISTS user_provider_early_access;

CREATE INDEX IF NOT EXISTS chapters_source_floor_num_idx
    ON chapters (series_source_id, (floor(number)), number);

DROP INDEX IF EXISTS chapters_source_floor_num_access_idx;

ALTER TABLE chapters DROP CONSTRAINT IF EXISTS chapters_unlocks_at_requires_early_access;
ALTER TABLE chapters DROP COLUMN IF EXISTS unlocks_at;
ALTER TABLE chapters DROP COLUMN IF EXISTS access;

DROP TYPE IF EXISTS chapter_access;
