-- Reverses 0032. The columns go before the type they are declared as.
ALTER TABLE series
  DROP COLUMN IF EXISTS title_source,
  DROP COLUMN IF EXISTS description_source,
  DROP COLUMN IF EXISTS cover_source,
  DROP COLUMN IF EXISTS content_type_source,
  DROP COLUMN IF EXISTS status_source,
  DROP COLUMN IF EXISTS release_year_source;

DROP TYPE IF EXISTS metadata_source;
