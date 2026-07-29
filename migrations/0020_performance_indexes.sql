-- Indexes for the hot paths that were doing full scans and full sorts (audit: PERFORMANCE §2,
-- §4, §12, and the missing-index table).
--
-- ---------------------------------------------------------------------------------------
-- Why these are NOT `CREATE INDEX CONCURRENTLY`
-- ---------------------------------------------------------------------------------------
-- They cannot be, in this migrator. sqlx's `-- no-transaction` directive does suppress the
-- *explicit* transaction, but sqlx sends the whole file as one simple-query string and
-- Postgres wraps any multi-statement simple query in an **implicit** transaction — so
-- CONCURRENTLY still fails with "cannot run inside a transaction block". The only ways around
-- it are one statement per migration file (nine files for nine indexes) or dropping
-- CONCURRENTLY, and nine files to express one change is worse for every future reader.
--
-- A plain `CREATE INDEX` takes an ACCESS EXCLUSIVE lock for the duration of the build, which
-- is milliseconds on a small table and minutes of downtime on a large one.
--
-- **On a database whose `series` or `chapters` tables are already large, run the block below
-- by hand before deploying this migration.** Every statement here is `IF NOT EXISTS`, so the
-- migration then finds the indexes present and does nothing:
--
--   CREATE INDEX CONCURRENTLY IF NOT EXISTS series_updated_idx             ON series (updated_at DESC, id DESC);
--   CREATE INDEX CONCURRENTLY IF NOT EXISTS series_enrichment_cursor_idx   ON series (updated_at ASC, id ASC);
--   CREATE INDEX CONCURRENTLY IF NOT EXISTS series_title_sort_idx          ON series (canonical_title);
--   CREATE INDEX CONCURRENTLY IF NOT EXISTS series_release_year_idx        ON series (release_year) WHERE release_year IS NOT NULL;
--   CREATE INDEX CONCURRENTLY IF NOT EXISTS series_created_idx             ON series (created_at DESC, id DESC);
--   CREATE INDEX CONCURRENTLY IF NOT EXISTS chapters_source_floor_idx      ON chapters (series_source_id, (floor(number)));
--   CREATE INDEX CONCURRENTLY IF NOT EXISTS chapters_source_disc_idx       ON chapters (series_source_id, discovered_at DESC);
--   CREATE INDEX CONCURRENTLY IF NOT EXISTS notifications_user_all_idx     ON notifications (user_id, created_at DESC);
--   CREATE INDEX CONCURRENTLY IF NOT EXISTS watchlist_user_added_idx       ON watchlist_entries (user_id, added_at DESC);
--
-- If a CONCURRENTLY build is interrupted it leaves an INVALID index the planner ignores:
--   SELECT indexrelid::regclass FROM pg_index WHERE NOT indisvalid;
--   REINDEX INDEX CONCURRENTLY <name>;

-- ---------------------------------------------------------------------------------------
-- series
-- ---------------------------------------------------------------------------------------

-- Backs three distinct hot paths: the Discover browse listing's default `ORDER BY
-- s.updated_at DESC`, the admin listing's, and the enrichment sweep's ascending walk. Only
-- `series_search_gin`, `series_title_trgm` and `series_status_idx` existed, none of which can
-- serve an ordering.
CREATE INDEX IF NOT EXISTS series_updated_idx
    ON series (updated_at DESC, id DESC);

-- The *ascending* half of the same key, for the enrichment cursor. A descending index can be
-- scanned backwards, but a dedicated ascending one keeps the keyset seek a plain forward scan
-- and costs little on a table this shape.
CREATE INDEX IF NOT EXISTS series_enrichment_cursor_idx
    ON series (updated_at ASC, id ASC);

-- `sort=title` sorted the whole matching set with no btree to lean on.
CREATE INDEX IF NOT EXISTS series_title_sort_idx
    ON series (canonical_title);

-- The `year_min`/`year_max` range filters and `sort=year`. Partial, because a series with no
-- release year can never satisfy a range predicate and only bloats the index.
CREATE INDEX IF NOT EXISTS series_release_year_idx
    ON series (release_year)
    WHERE release_year IS NOT NULL;

-- The keyset cursor `catalog.rs` documents for deep pagination.
CREATE INDEX IF NOT EXISTS series_created_idx
    ON series (created_at DESC, id DESC);

-- ---------------------------------------------------------------------------------------
-- chapters
-- ---------------------------------------------------------------------------------------

-- An *expression* index. The tracking queries compare `floor(number)` — chapter 10.5 is a part
-- of chapter 10, so progress counts by whole chapters — and `chapters_source_idx
-- (series_source_id, number DESC)` cannot serve a predicate on a function of the column.
CREATE INDEX IF NOT EXISTS chapters_source_floor_idx
    ON chapters (series_source_id, (floor(number)));

-- `chapters_discovered (discovered_at DESC)` is global. The correlated `max(c.discovered_at)`
-- per source in the watchlist ordering needs it per source, or it reads every chapter row of
-- every source it touches.
CREATE INDEX IF NOT EXISTS chapters_source_disc_idx
    ON chapters (series_source_id, discovered_at DESC);

-- ---------------------------------------------------------------------------------------
-- notifications / watchlist
-- ---------------------------------------------------------------------------------------

-- `notifications_user_unread` is partial (`WHERE read_at IS NULL`). The list endpoint returns
-- read *and* unread rows, so it cannot use that index at all.
CREATE INDEX IF NOT EXISTS notifications_user_all_idx
    ON notifications (user_id, created_at DESC);

-- The primary key is `(user_id, series_id)`, so the sort key was unindexed.
CREATE INDEX IF NOT EXISTS watchlist_user_added_idx
    ON watchlist_entries (user_id, added_at DESC);

-- ---------------------------------------------------------------------------------------
-- Deliberately NOT added: series_tags (tag_id, series_id)
-- ---------------------------------------------------------------------------------------
-- The audit marks it UNVERIFIED. The primary key is `(series_id, tag_id)`, so a reverse lookup
-- has no leading-column btree — but Postgres can and often does answer it from the PK index
-- with a bitmap scan, and `series_tags` is a narrow table where that is cheap. Run `EXPLAIN
-- (ANALYZE, BUFFERS)` on the `liked_tags` CTE against production-shaped data before adding an
-- index whose only justification is a guess about the planner.
--
