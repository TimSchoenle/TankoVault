-- The unread predicate could not be answered from an index alone.
--
-- ---------------------------------------------------------------------------------------
-- What was wrong
-- ---------------------------------------------------------------------------------------
-- The predicate spelled out in `repo::tracking::dashboard` reads *two* things from `chapters`:
--
--   floor(c.number) > COALESCE(rp.last_read_whole_number, 0)
--     AND NOT (c.number <> floor(c.number)
--              AND rp.last_read_part_number IS NOT NULL
--              AND c.number <= rp.last_read_part_number)
--
-- `chapters_source_floor_idx (series_source_id, (floor(number)))` carries only the first, so
-- the sub-chapter half of the test — which exists because chapter 10.5 is a *part* of chapter
-- 10 and must not re-appear as unread once it is read — sent every candidate row to the heap.
-- On the Home stats query that is one heap fetch per chapter of every watched series: 2.9 s
-- measured while a scan was running.
--
-- Adding `number` as a third key column makes the whole predicate index-resolvable, so the
-- scan is index-only for readers whose watched sources are visibility-mapped. `chapters` is
-- insert-heavy, so that last part depends on autovacuum keeping the map current — the index is
-- still the right shape when it does not, it simply pays the heap fetches it pays today.
--
-- The two-column index is superseded and dropped: this one has the same leading columns, so
-- every plan that used the old one uses this. Ordering the keys the other way round would not
-- work — `series_source_id` must lead, because every one of these queries reaches `chapters`
-- through a known set of sources.
--
-- ---------------------------------------------------------------------------------------
-- Why this is NOT `CREATE INDEX CONCURRENTLY`
-- ---------------------------------------------------------------------------------------
-- Same reason as `0020_performance_indexes`, which explains it at length: sqlx sends the file
-- as one simple-query string and Postgres wraps that in an implicit transaction. On a database
-- whose `chapters` table is already large, run this by hand before deploying — the statements
-- below are `IF NOT EXISTS`/`IF EXISTS`, so the migration then finds the work done:
--
--   CREATE INDEX CONCURRENTLY IF NOT EXISTS chapters_source_floor_num_idx
--       ON chapters (series_source_id, (floor(number)), number);
--   DROP INDEX CONCURRENTLY IF EXISTS chapters_source_floor_idx;

CREATE INDEX IF NOT EXISTS chapters_source_floor_num_idx
    ON chapters (series_source_id, (floor(number)), number);

DROP INDEX IF EXISTS chapters_source_floor_idx;
