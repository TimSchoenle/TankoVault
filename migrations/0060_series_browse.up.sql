-- The browse keys of every series in one narrow row, kept current by triggers.
--
-- Design and measurements: docs/perf/BROWSE_REDESIGN.md. The Discover count read the whole
-- `series` heap (60 MB) for every filter, evaluated the include-tags `EXCEPT` and the
-- min-chapters `max()` once per series, and sorted by chapters or sources after running those
-- aggregates for every matching row. Every filter and sort key now lives here, with the tag and
-- provider sets as arrays a GIN index can answer.
--
-- ---------------------------------------------------------------------------------------
-- The invariant
-- ---------------------------------------------------------------------------------------
-- For every series S there is exactly one row, and each column equals its definition over the
-- base tables:
--   updated_at, content_type, status, release_year, adult_gated, canonical_title = S's own;
--   max_chapters = COALESCE(max(chapter_count), 0) over S's sources;
--   source_count = count(DISTINCT provider_id) over S's sources;
--   provider_ids = the sorted distinct provider ids of S's sources;
--   tag_ids      = the sorted tag ids of S's tags.
-- `repo::catalog::browse::verify_projection` re-checks rows in batches and repairs any that
-- disagree.
--
-- ---------------------------------------------------------------------------------------
-- Ordering constraint: lock, then compute
-- ---------------------------------------------------------------------------------------
-- The refresh functions lock the row before computing it, in a separate statement. Computing in
-- the same statement as the write would take the snapshot before waiting for the lock: two scans
-- of one series' sources committing close together would then each compute from a snapshot
-- without the other's change, and the second to commit would store a stale `max_chapters`.
--
-- Row-level triggers, not statement-level: every writer on the ingest path touches one series at
-- a time, where a row trigger measured cheapest. A bulk tag cleanup pays ~18 µs per series.
--
-- ---------------------------------------------------------------------------------------
-- Lock impact
-- ---------------------------------------------------------------------------------------
-- `CREATE TRIGGER` takes SHARE ROW EXCLUSIVE on `series`, `series_sources` and `series_tags`
-- until this migration commits. Reads continue; ingest, enrichment and merges wait for the
-- backfill (about 1.5 s on a 54 000-series catalogue).
--
-- All three locks are taken up front, and never while waiting holding another. Taking them one
-- `CREATE` at a time held `series` while waiting for `series_sources`; a running worker holding
-- `series_sources` and then writing `series` closed the cycle, and Postgres aborted the migration
-- with `deadlock detected` on every retry. Only the `series` lock waits, holding nothing; the other
-- two are `NOWAIT`, and a refusal rolls the attempt back — releasing `series` so that worker can
-- finish — and tries again.

DO $$
BEGIN
  FOR attempt IN 1..600 LOOP
    BEGIN
      LOCK TABLE series IN SHARE ROW EXCLUSIVE MODE;
      LOCK TABLE series_sources, series_tags IN SHARE ROW EXCLUSIVE MODE NOWAIT;
      RETURN;
    EXCEPTION WHEN lock_not_available THEN
      PERFORM pg_sleep(0.1);
    END;
  END LOOP;
  RAISE EXCEPTION 'series_browse: series_sources/series_tags stayed locked for 600 attempts';
END
$$;

CREATE TABLE series_browse (
  series_id       uuid          PRIMARY KEY REFERENCES series (id) ON DELETE CASCADE,
  updated_at      timestamptz   NOT NULL,
  content_type    content_type  NOT NULL,
  status          series_status NOT NULL,
  release_year    int,
  adult_gated     boolean       NOT NULL,
  canonical_title text          NOT NULL,
  max_chapters    int           NOT NULL DEFAULT 0,
  source_count    int           NOT NULL DEFAULT 0,
  provider_ids    uuid[]        NOT NULL DEFAULT '{}',
  tag_ids         uuid[]        NOT NULL DEFAULT '{}'
);

-- Recompute a series' source-derived columns under its row lock.
CREATE FUNCTION series_browse_refresh_sources(p_series uuid) RETURNS void
LANGUAGE plpgsql AS $$
BEGIN
  PERFORM 1 FROM series_browse WHERE series_id = p_series FOR UPDATE;
  UPDATE series_browse sb SET
    max_chapters = COALESCE(a.max_chapters, 0),
    source_count = COALESCE(a.source_count, 0),
    provider_ids = COALESCE(a.provider_ids, '{}')
  FROM (SELECT max(chapter_count) AS max_chapters,
               count(DISTINCT provider_id)::int AS source_count,
               array_agg(DISTINCT provider_id ORDER BY provider_id) AS provider_ids
        FROM series_sources WHERE series_id = p_series) a
  WHERE sb.series_id = p_series
    AND (sb.max_chapters, sb.source_count, sb.provider_ids)
        IS DISTINCT FROM (COALESCE(a.max_chapters, 0), COALESCE(a.source_count, 0),
                          COALESCE(a.provider_ids, '{}'));
END
$$;

-- Recompute a series' tag set under its row lock.
CREATE FUNCTION series_browse_refresh_tags(p_series uuid) RETURNS void
LANGUAGE plpgsql AS $$
BEGIN
  PERFORM 1 FROM series_browse WHERE series_id = p_series FOR UPDATE;
  UPDATE series_browse sb SET tag_ids = t.tag_ids
  FROM (SELECT COALESCE(array_agg(tag_id ORDER BY tag_id), '{}') AS tag_ids
        FROM series_tags WHERE series_id = p_series) t
  WHERE sb.series_id = p_series AND sb.tag_ids IS DISTINCT FROM t.tag_ids;
END
$$;

-- Replace the rows of the named series with a recomputation from the base tables, one series at
-- a time under its row lock. Creates missing rows and removes rows whose series is gone.
CREATE FUNCTION series_browse_rebuild(p_series uuid[]) RETURNS void
LANGUAGE plpgsql AS $$
DECLARE
  s uuid;
BEGIN
  FOREACH s IN ARRAY (SELECT array_agg(x ORDER BY x) FROM unnest(p_series) x) LOOP
    INSERT INTO series_browse
      (series_id, updated_at, content_type, status, release_year, adult_gated, canonical_title)
    SELECT id, updated_at, content_type, status, release_year, adult_gated, canonical_title
    FROM series WHERE id = s
    ON CONFLICT (series_id) DO UPDATE SET
      updated_at = EXCLUDED.updated_at, content_type = EXCLUDED.content_type,
      status = EXCLUDED.status, release_year = EXCLUDED.release_year,
      adult_gated = EXCLUDED.adult_gated, canonical_title = EXCLUDED.canonical_title;
    DELETE FROM series_browse sb
    WHERE sb.series_id = s AND NOT EXISTS (SELECT 1 FROM series WHERE id = s);
    PERFORM series_browse_refresh_sources(s);
    PERFORM series_browse_refresh_tags(s);
  END LOOP;
END
$$;

-- ---- series ------------------------------------------------------------------------------

CREATE FUNCTION series_browse_series_changed() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  INSERT INTO series_browse
    (series_id, updated_at, content_type, status, release_year, adult_gated, canonical_title)
  VALUES (NEW.id, NEW.updated_at, NEW.content_type, NEW.status, NEW.release_year,
          NEW.adult_gated, NEW.canonical_title)
  ON CONFLICT (series_id) DO UPDATE SET
    updated_at = EXCLUDED.updated_at, content_type = EXCLUDED.content_type,
    status = EXCLUDED.status, release_year = EXCLUDED.release_year,
    adult_gated = EXCLUDED.adult_gated, canonical_title = EXCLUDED.canonical_title;
  RETURN NULL;
END
$$;

CREATE TRIGGER series_browse_series_insert AFTER INSERT ON series
  FOR EACH ROW EXECUTE FUNCTION series_browse_series_changed();
-- The `WHEN` is what keeps an enrichment pass that rewrites only descriptions or covers free.
CREATE TRIGGER series_browse_series_update AFTER UPDATE ON series
  FOR EACH ROW
  WHEN ((OLD.updated_at, OLD.content_type, OLD.status, OLD.release_year, OLD.adult_gated,
         OLD.canonical_title)
        IS DISTINCT FROM
        (NEW.updated_at, NEW.content_type, NEW.status, NEW.release_year, NEW.adult_gated,
         NEW.canonical_title))
  EXECUTE FUNCTION series_browse_series_changed();

-- ---- series_sources ----------------------------------------------------------------------

CREATE FUNCTION series_browse_sources_changed() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  IF TG_OP IN ('INSERT', 'UPDATE') THEN
    PERFORM series_browse_refresh_sources(NEW.series_id);
  END IF;
  IF TG_OP = 'DELETE' OR (TG_OP = 'UPDATE' AND OLD.series_id <> NEW.series_id) THEN
    PERFORM series_browse_refresh_sources(OLD.series_id);
  END IF;
  RETURN NULL;
END
$$;

CREATE TRIGGER series_browse_sources_insert AFTER INSERT ON series_sources
  FOR EACH ROW EXECUTE FUNCTION series_browse_sources_changed();
CREATE TRIGGER series_browse_sources_delete AFTER DELETE ON series_sources
  FOR EACH ROW EXECUTE FUNCTION series_browse_sources_changed();
-- Every scan rewrites `last_scanned_at` and `content_hash`; only these three columns matter.
CREATE TRIGGER series_browse_sources_update AFTER UPDATE ON series_sources
  FOR EACH ROW
  WHEN ((OLD.series_id, OLD.provider_id, OLD.chapter_count)
        IS DISTINCT FROM (NEW.series_id, NEW.provider_id, NEW.chapter_count))
  EXECUTE FUNCTION series_browse_sources_changed();

-- ---- series_tags -------------------------------------------------------------------------

CREATE FUNCTION series_browse_tags_changed() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  IF TG_OP IN ('INSERT', 'UPDATE') THEN
    PERFORM series_browse_refresh_tags(NEW.series_id);
  END IF;
  IF TG_OP = 'DELETE' OR (TG_OP = 'UPDATE' AND OLD.series_id <> NEW.series_id) THEN
    PERFORM series_browse_refresh_tags(OLD.series_id);
  END IF;
  RETURN NULL;
END
$$;

CREATE TRIGGER series_browse_tags_insert AFTER INSERT ON series_tags
  FOR EACH ROW EXECUTE FUNCTION series_browse_tags_changed();
CREATE TRIGGER series_browse_tags_delete AFTER DELETE ON series_tags
  FOR EACH ROW EXECUTE FUNCTION series_browse_tags_changed();
CREATE TRIGGER series_browse_tags_update AFTER UPDATE ON series_tags
  FOR EACH ROW
  WHEN ((OLD.series_id, OLD.tag_id) IS DISTINCT FROM (NEW.series_id, NEW.tag_id))
  EXECUTE FUNCTION series_browse_tags_changed();

-- ---- backfill ----------------------------------------------------------------------------

INSERT INTO series_browse
SELECT s.id, s.updated_at, s.content_type, s.status, s.release_year, s.adult_gated,
       s.canonical_title,
       COALESCE(src.max_chapters, 0), COALESCE(src.source_count, 0),
       COALESCE(src.provider_ids, '{}'), COALESCE(tg.tag_ids, '{}')
FROM series s
LEFT JOIN (SELECT series_id, max(chapter_count) AS max_chapters,
                  count(DISTINCT provider_id)::int AS source_count,
                  array_agg(DISTINCT provider_id ORDER BY provider_id) AS provider_ids
           FROM series_sources GROUP BY series_id) src ON src.series_id = s.id
LEFT JOIN (SELECT series_id, array_agg(tag_id ORDER BY tag_id) AS tag_ids
           FROM series_tags GROUP BY series_id) tg ON tg.series_id = s.id;

-- The sort-token statement names one key per order; a custom plan folds the other `CASE` arms
-- away, so each order walks its own index.
CREATE INDEX series_browse_updated_idx ON series_browse (updated_at DESC, series_id DESC);
CREATE INDEX series_browse_title_idx
  ON series_browse (canonical_title, updated_at DESC, series_id DESC);
CREATE INDEX series_browse_year_idx
  ON series_browse (release_year DESC NULLS LAST, updated_at DESC, series_id DESC);
CREATE INDEX series_browse_chapters_idx
  ON series_browse (max_chapters DESC NULLS LAST, updated_at DESC, series_id DESC);
CREATE INDEX series_browse_sources_idx
  ON series_browse (source_count DESC NULLS LAST, updated_at DESC, series_id DESC);
CREATE INDEX series_browse_tags_gin ON series_browse USING gin (tag_ids);
CREATE INDEX series_browse_providers_gin ON series_browse USING gin (provider_ids);

ANALYZE series_browse;
