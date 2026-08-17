-- Chapter storage: half the bytes, a third of the indexes.
--
-- `chapters` is the largest table in the deployment — `deploy/docker-compose.yml` records that it
-- "passed 1.2 GB" while sizing Postgres, against a container capped at 1 GB with 256 MB of
-- `shared_buffers`. That is not a disk-cost problem, it is a cache-residency one: the tuning
-- comment in that file already names the symptom, "a running scan's chapter inserts evict the
-- working set continuously", and the 40–140 ms warm statements that took 1.0–3.3 s cold.
--
-- Measured on 1.2 M rows (see `docs/CHAPTER_STORAGE.md` for the method and the full breakdown):
-- 398.2 bytes/row, of which **indexes were 64%** — six of them, three indexing some form of
-- `(series_source_id, number)`. This migration takes that to 201.7 bytes/row.
--
-- Four things change, and they are not independent — each one is what makes the next possible.
--
-- 1. `number numeric(10,4)` becomes `number_milli int` (the number scaled by 10 000).
-- 2. That collapses four indexes into one (see below).
-- 3. `id uuid` goes: nothing referenced it.
-- 4. `path` is stored relative to the source's own path, and `volume` is dropped.

-- ---------------------------------------------------------------------------------------
-- Why the number becomes an integer
-- ---------------------------------------------------------------------------------------
-- Not because `numeric` is wasteful — at these magnitudes it occupies the same 8 bytes after
-- alignment that an `int` plus padding does. Because **`floor(number)` is not derivable from an
-- index on `number`**, and the unread predicate every reading surface runs is written in terms of
-- `floor`. Migration 0026 built a second index on `(series_source_id, (floor(number)))` to make it
-- reachable, and documents at length what it cost when it was not: 2.9 s on the Home stats query.
-- 0047 then widened that index again with `INCLUDE (access, unlocks_at)` to keep the scan
-- index-only.
--
-- Scaled to an integer, `floor(number) > w` is `number_milli >= (w + 1) * 10000` — a plain range
-- on the second key column of the `(series_source_id, number_milli)` index. The `floor` index has
-- nothing left to do. And because a **unique** index may carry `INCLUDE` columns, one index now
-- does the work of four:
--
--   chapters_pkey                            31.6 B/row
--   chapters_series_source_id_number_key     40.6
--   chapters_source_idx                      40.6   (a duplicate of the line above: a btree scans
--                                                    backwards, so the DESC bought nothing)
--   chapters_source_floor_num_access_idx     58.9
--   ------------------------------------------------
--   replaced by chapters_source_number_key   49.7   → net −122.0 B/row
--
-- The scale of 10 000 is the precision `numeric(10,4)` carried, which is what part releases
-- (`152.1`, `152.65`) need. It caps the chapter number at `i32::MAX / 10000`; the domain rounds
-- that down to 200 000 (`tankovault_domain::chapter_number::MAX_CHAPTER_NUMBER`), roughly fifty
-- times the longest series anyone has published.

-- ---------------------------------------------------------------------------------------
-- Why the path is stored relative to the source
-- ---------------------------------------------------------------------------------------
-- `path` is the largest variable field in the row — mean 42.4 characters — and for most providers
-- it is the series path with a few characters appended: `/manga/<slug>/chapter-1050/` under a
-- source whose `source_path` is `/manga/<slug>`. That prefix is already stored, once per source,
-- in a table every chapter query already joins.
--
-- Not every provider nests that way. MangaDex's series path is `/title/{uuid}` and its chapter
-- path is `/chapter/{uuid}` — no shared prefix at all. So the encoding carries its own
-- discriminator instead of a column: **a stored value beginning with `/` is site-relative and was
-- stored whole; anything else is relative to `source_path`.** Every writer goes through
-- `adapters::relativize`, which guarantees the leading slash, so the two cases cannot collide.
-- `chapter_url_path` below is the SQL half of that; `tankovault_domain::link` is the Rust half.

-- ---------------------------------------------------------------------------------------
-- A table rewrite, not a sequence of ALTERs
-- ---------------------------------------------------------------------------------------
-- Changing a column type, dropping three columns and reordering the rest would be four rewrites
-- done as `ALTER`s. Built as a new table it is one, and the column order can be chosen: the three
-- 8-byte-aligned timestamps lead, then the uuid, then the two 4-byte fields, then the varlenas.
-- That removes the ~4 bytes/row of alignment padding the old order carried.
--
-- sqlx sends this file as one simple-query string, which Postgres wraps in an implicit
-- transaction, so the swap is atomic. It is **not** online: the rewrite holds an
-- `ACCESS EXCLUSIVE` lock for its duration. On a large deployment, do it in a maintenance window.

-- The SQL half of the path encoding. `IMMUTABLE` so it can be used in an index expression later
-- if that ever becomes necessary, and so the planner may fold it.
CREATE OR REPLACE FUNCTION chapter_url_path(source_path text, stored text)
RETURNS text
LANGUAGE sql
IMMUTABLE
PARALLEL SAFE
RETURNS NULL ON NULL INPUT
AS $$
  SELECT CASE
    WHEN left(stored, 1) = '/' THEN stored
    ELSE rtrim(source_path, '/') || '/' || stored
  END
$$;

CREATE TABLE chapters_new (
  -- 8-byte aligned first, so nothing below needs padding to reach its boundary.
  published_at     timestamptz,
  discovered_at    timestamptz NOT NULL DEFAULT now(),
  unlocks_at       timestamptz,
  series_source_id uuid NOT NULL REFERENCES series_sources(id) ON DELETE CASCADE,
  -- The chapter number, scaled by 10 000.
  number_milli     int NOT NULL,
  access           chapter_access NOT NULL DEFAULT 'free',
  title            text,
  -- Relative to `series_sources.source_path` unless it begins with `/`. See above.
  path             text NOT NULL,
  -- Non-negative is not a style preference: the read paths spell `floor(number)` as the integer
  -- division `number_milli / 10000`, which only equals `floor` for non-negative values. A single
  -- negative row would silently mis-place every reader's progress against that series.
  CONSTRAINT chapters_number_milli_range
    CHECK (number_milli >= 0 AND number_milli <= 2000000000),
  -- Carried over verbatim from 0047: a free chapter cannot hold an unlock time, because the read
  -- predicate treats a non-null `unlocks_at` as authoritative.
  CONSTRAINT chapters_unlocks_at_requires_early_access
    CHECK (access = 'early_access' OR unlocks_at IS NULL)
);

-- `round()` and not a truncating cast: `number::numeric * 10000` is exact for a `numeric(10,4)`,
-- but `round` states the intent and survives a value that somehow carried more scale.
--
-- Rows above the new ceiling are **dropped, not clamped**. They are junk by construction — the
-- ceiling is fifty times the longest real series — and clamping would fold a date-shaped slug like
-- `chapter-180302` onto chapter 200 000, which a reader's progress would then be measured against.
-- The count is reported so an operator sees whether anything was lost.
DO $$
DECLARE dropped bigint;
BEGIN
  SELECT count(*) INTO dropped FROM chapters WHERE number < 0 OR number > 200000;
  IF dropped > 0 THEN
    RAISE NOTICE 'dropping % chapter row(s) outside the storable range 0..200000', dropped;
  END IF;
END $$;

INSERT INTO chapters_new
  (published_at, discovered_at, unlocks_at, series_source_id, number_milli, access, title, path)
SELECT c.published_at,
       c.discovered_at,
       c.unlocks_at,
       c.series_source_id,
       round(c.number * 10000)::int,
       c.access,
       c.title,
       -- The inverse of `chapter_url_path`: strip the source's own prefix when it is one, keep the
       -- leading slash otherwise so the value stays self-describing.
       CASE
         WHEN c.path LIKE rtrim(ss.source_path, '/') || '/%'
           THEN substr(c.path, length(rtrim(ss.source_path, '/')) + 2)
         ELSE c.path
       END
FROM chapters c
JOIN series_sources ss ON ss.id = c.series_source_id
WHERE c.number >= 0 AND c.number <= 200000;

DROP TABLE chapters;
ALTER TABLE chapters_new RENAME TO chapters;

-- ---------------------------------------------------------------------------------------
-- The two indexes that remain
-- ---------------------------------------------------------------------------------------
-- One unique covering index replaces four. It enforces uniqueness, is the `ON CONFLICT` arbiter,
-- serves `ORDER BY number_milli DESC` by scanning backwards, and answers the unread predicate
-- **index-only** — which is the property 0026 and 0047 exist to protect.
CREATE UNIQUE INDEX chapters_source_number_key
    ON chapters (series_source_id, number_milli) INCLUDE (access, unlocks_at);

-- Postgres accepts a unique index with `INCLUDE` columns as the backing index for a primary key,
-- so the table keeps a real primary key rather than a bare unique constraint.
ALTER TABLE chapters ADD PRIMARY KEY USING INDEX chapters_source_number_key;

-- Kept from 0052: the release feed and the "last updated" aggregates order by discovery within a
-- source, which the key above cannot answer.
CREATE INDEX chapters_source_disc_access_idx
    ON chapters (series_source_id, discovered_at DESC) INCLUDE (access, unlocks_at);

-- `chapters_discovered (discovered_at DESC)` is deliberately **not** recreated. Its only readers
-- were the three "chapters discovered in the last hour/day/week" counts on the admin overview
-- (`repo::stats`), and Postgres 18's btree skip scan answers those from the per-source index
-- above by skipping the leading column. Measured: 0.14 ms with the dedicated index against
-- 19.8 ms without it, on a query that sits behind a 30 s cache (`ADMIN_STATS_TTL`). 22.5 bytes a
-- row is the better side of that trade.
--
-- A BRIN index was measured as an alternative and rejected: it costs 24 kB but the planner
-- prefers the skip scan over it anyway, so it would be a maintained index that answers nothing.

ANALYZE chapters;
