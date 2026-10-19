-- A source's listing title as a match key, and the provenance of every alternative title.
--
-- `provider_title_normalized` is what the provider calls the work, normalized like every other
-- key. A listing title is a primary title: a mirror lists the same catalogue under the same
-- names, so matching on it attaches a mirror to the series its siblings already sit under, even
-- after AniList has renamed that series. It is filled from Rust (`normalize_title`) by every
-- write path and by the `rebuild_normalized_keys` operator action, which must be run once after
-- this migration: an SQL twin of the normalizer would drift from it.
ALTER TABLE series_sources ADD COLUMN provider_title_normalized text;

-- Equality on the compact key, mirroring `series_compact_title_idx` (0023). Not `CONCURRENTLY`,
-- for the reason `0020_performance_indexes` gives; on a large database run it by hand first.
CREATE INDEX IF NOT EXISTS series_sources_compact_title_idx
    ON series_sources (replace(provider_title_normalized, ' ', ''))
    WHERE provider_title_normalized IS NOT NULL;

-- Where an alternative title came from, so a cleanup can target one provenance: a scan's
-- adapter, an AniList enrichment, or a merge that copied it from the absorbed series. Rows that
-- predate the column are recorded as 'adapter', which is what nearly all of them are.
ALTER TABLE series_titles ADD COLUMN origin text NOT NULL DEFAULT 'adapter'
    CONSTRAINT series_titles_origin_check CHECK (origin IN ('adapter', 'anilist', 'merge'));
