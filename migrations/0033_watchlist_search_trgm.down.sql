-- Reverses 0033. The normalized-column trigram indexes from 0003 are untouched.
DROP INDEX IF EXISTS series_canonical_title_trgm;
DROP INDEX IF EXISTS series_titles_title_trgm;
DROP INDEX IF EXISTS tags_name_trgm;
DROP INDEX IF EXISTS authors_name_trgm;
