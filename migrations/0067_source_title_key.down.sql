ALTER TABLE series_titles DROP COLUMN IF EXISTS origin;
DROP INDEX IF EXISTS series_sources_compact_title_idx;
ALTER TABLE series_sources DROP COLUMN IF EXISTS provider_title_normalized;
