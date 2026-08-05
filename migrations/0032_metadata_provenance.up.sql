-- Per-field metadata provenance: which source wrote the value that is stored.
--
-- `metadata.priority` is a per-field ordering, so a writer can only honour it if it can tell
-- who wrote the incumbent value. Without that, the last writer won: the hourly catalogue scan
-- overwrote every AniList description, and reset `content_type`/`status` to the `unknown` its
-- adapters hardcode, until the next enrichment sweep put them back.
CREATE TYPE metadata_source AS ENUM ('anilist', 'adapter');

-- Nullable on purpose. NULL is "written before provenance was recorded", which the merge reads
-- as `adapter` — the conservative assumption, since it lets a higher-priority source correct the
-- row on its next pass rather than freezing whatever happened to be there.
ALTER TABLE series
  ADD COLUMN IF NOT EXISTS title_source        metadata_source,
  ADD COLUMN IF NOT EXISTS description_source  metadata_source,
  ADD COLUMN IF NOT EXISTS cover_source        metadata_source,
  ADD COLUMN IF NOT EXISTS content_type_source metadata_source,
  ADD COLUMN IF NOT EXISTS status_source       metadata_source,
  ADD COLUMN IF NOT EXISTS release_year_source metadata_source;
