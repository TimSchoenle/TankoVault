-- Watchlist free-text search had no index it could use.
--
-- The predicate in `repo::tracking::watchlist` searched with `strpos(lower(col), lower($n)) > 0`,
-- which no index type serves: `gin_trgm_ops` answers LIKE/ILIKE, not `strpos`. The two trigram
-- indexes that already exist (`series_title_trgm`, `series_titles_trgm`, both from
-- `0003_catalog`) are on the *normalized* columns, which that predicate never touched, and
-- `tags.name`/`authors.name` had no text index at all. Every watchlist search therefore ran three
-- correlated subqueries per watchlist row against unindexed scans.
--
-- The predicate is now `col ILIKE '%term%'`, so these four indexes are the ones it can use — one
-- per column it actually searches. They are on the raw display columns rather than the normalized
-- ones on purpose: the search matches what the user sees, and pg_trgm lowercases when it extracts
-- trigrams, so an ILIKE probe hits the same index entries a LIKE would.
--
-- Not `CREATE INDEX CONCURRENTLY`, for the reason `0020_performance_indexes` sets out at length:
-- sqlx sends the file as one simple-query string and Postgres wraps it in an implicit
-- transaction. On a large catalogue, build them by hand first — the statements below are
-- `IF NOT EXISTS`, so the migration then finds the work done:
--
--   CREATE INDEX CONCURRENTLY IF NOT EXISTS series_canonical_title_trgm
--       ON series USING gin (canonical_title gin_trgm_ops);
--   (and the three below, likewise)

CREATE INDEX IF NOT EXISTS series_canonical_title_trgm
    ON series USING gin (canonical_title gin_trgm_ops);

CREATE INDEX IF NOT EXISTS series_titles_title_trgm
    ON series_titles USING gin (title gin_trgm_ops);

CREATE INDEX IF NOT EXISTS tags_name_trgm
    ON tags USING gin (name gin_trgm_ops);

CREATE INDEX IF NOT EXISTS authors_name_trgm
    ON authors USING gin (name gin_trgm_ops);
