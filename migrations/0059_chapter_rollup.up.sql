-- Chapter counts per source and discovery instant, kept exact by triggers.
--
-- The console header, the per-provider table and the purge panel counted `chapters` whole on
-- every refresh: 7.8–48.7 s in production. They now sum this table, whose size follows the
-- number of ingest statements in the last week plus one row per source, not the chapter count.
--
-- ---------------------------------------------------------------------------------------
-- The invariant
-- ---------------------------------------------------------------------------------------
-- For every source S: sum(chapters) over S's rows = count(*) of S's chapters, and for any
-- instant T no more than seven days back, sum(chapters) over S's rows with discovered_at > T =
-- count(*) of S's chapters with discovered_at > T. Rows keep the exact discovery instant until
-- it is older than seven days; then they are folded into S's `-infinity` row, which keeps only
-- the total and the newest instant it absorbed. Every 1 h / 24 h / 7 d window is therefore
-- exact, and so is every total.
--
-- `chapter_rollup_apply` is the only writer. It locks the source's rows it may touch in key
-- order before reading them, because a decrement has to find the row its chapter was counted
-- in, and a fold running concurrently for the same source may be moving that row to
-- `-infinity`. Without the lock the decrement is lost; with an unordered lock the two deadlock.
--
-- A source's rows cascade with the source. A chapter deleted by that cascade then finds no row
-- to decrement, which is correct: the whole count went with the source.
--
-- `repo::catalog::rollup` verifies sources in batches against a live count and rebuilds any
-- that disagree through `chapter_rollup_rebuild`; drift means a writer bypassed the triggers
-- (`session_replication_role = replica`, a restore).
--
-- ---------------------------------------------------------------------------------------
-- Lock impact
-- ---------------------------------------------------------------------------------------
-- `CREATE TRIGGER` takes SHARE ROW EXCLUSIVE on `chapters` and `series_sources` until this
-- migration commits, and the backfill reads `chapters` whole. Reads continue; ingest, merges and
-- deletes wait for the backfill.

CREATE TABLE chapter_rollup (
  series_source_id   uuid        NOT NULL REFERENCES series_sources (id) ON DELETE CASCADE,
  -- Copied from the source so the per-provider sum needs no join; kept current by trigger.
  provider_id        uuid        NOT NULL,
  -- The exact discovery instant, or `-infinity` for the source's folded history.
  discovered_at      timestamptz NOT NULL,
  chapters           int         NOT NULL,
  -- The newest discovery instant counted here; equal to `discovered_at` until folded.
  last_discovered_at timestamptz NOT NULL,
  PRIMARY KEY (series_source_id, discovered_at)
);

CREATE INDEX chapter_rollup_recent ON chapter_rollup (discovered_at) INCLUDE (chapters)
  WHERE discovered_at > '-infinity';

-- Apply signed chapter-count deltas, one per (source, discovery instant).
CREATE FUNCTION chapter_rollup_apply(p_source uuid[], p_at timestamptz[], p_delta int[])
RETURNS void
LANGUAGE plpgsql AS $$
DECLARE
  horizon constant timestamptz := now() - interval '7 days';
BEGIN
  IF cardinality(p_source) IS NULL OR cardinality(p_source) = 0 THEN
    RETURN;
  END IF;

  PERFORM 1 FROM chapter_rollup r
  WHERE r.series_source_id = ANY(p_source)
    AND (r.discovered_at < horizon OR r.discovered_at = ANY(p_at))
  ORDER BY r.series_source_id, r.discovered_at
  FOR UPDATE;

  INSERT INTO chapter_rollup AS r
    (series_source_id, provider_id, discovered_at, chapters, last_discovered_at)
  SELECT d.src, ss.provider_id, d.at, sum(d.delta)::int, d.at
  FROM unnest(p_source, p_at, p_delta) AS d(src, at, delta)
  JOIN series_sources ss ON ss.id = d.src
  WHERE d.delta > 0
  GROUP BY d.src, ss.provider_id, d.at
  ORDER BY d.src, d.at
  ON CONFLICT (series_source_id, discovered_at) DO UPDATE
    SET chapters = r.chapters + EXCLUDED.chapters;

  -- A decrement lands on the row its chapter was counted in: the exact instant, or the folded
  -- history once that instant has been folded.
  WITH gone AS (
    SELECT d.src, d.at, -sum(d.delta) AS n
    FROM unnest(p_source, p_at, p_delta) AS d(src, at, delta)
    WHERE d.delta < 0
    GROUP BY d.src, d.at
  ), target AS (
    SELECT g.src, COALESCE(exact.discovered_at, '-infinity') AS at, sum(g.n) AS n
    FROM gone g
    LEFT JOIN chapter_rollup exact
      ON exact.series_source_id = g.src AND exact.discovered_at = g.at
    GROUP BY g.src, COALESCE(exact.discovered_at, '-infinity')
  )
  UPDATE chapter_rollup r SET chapters = r.chapters - t.n
  FROM target t
  WHERE r.series_source_id = t.src AND r.discovered_at = t.at;

  DELETE FROM chapter_rollup r
  WHERE r.series_source_id = ANY(p_source) AND r.chapters = 0;

  -- Fold week-old instants of every source that just gained chapters. A source that stops
  -- gaining chapters keeps at most one week of unfolded rows from before it stopped.
  WITH aged AS (
    DELETE FROM chapter_rollup r
    WHERE r.series_source_id = ANY(ARRAY(
            SELECT DISTINCT d.src FROM unnest(p_source, p_delta) AS d(src, delta)
            WHERE d.delta > 0))
      AND r.discovered_at < horizon
      AND r.discovered_at > '-infinity'
    RETURNING r.series_source_id, r.provider_id, r.chapters, r.last_discovered_at
  )
  INSERT INTO chapter_rollup AS r
    (series_source_id, provider_id, discovered_at, chapters, last_discovered_at)
  SELECT a.series_source_id, a.provider_id, '-infinity', sum(a.chapters)::int,
         max(a.last_discovered_at)
  FROM aged a
  GROUP BY a.series_source_id, a.provider_id
  ORDER BY a.series_source_id
  ON CONFLICT (series_source_id, discovered_at) DO UPDATE
    SET chapters = r.chapters + EXCLUDED.chapters,
        last_discovered_at = GREATEST(r.last_discovered_at, EXCLUDED.last_discovered_at);
END
$$;

-- Replace the rows of the named sources with a recount from `chapters`.
--
-- The recount and the replacement are one MERGE, so they share a snapshot: a chapter committed
-- by a concurrent ingest is neither recounted here nor has its own row removed. The rows are
-- locked first, in the same order `chapter_rollup_apply` uses.
CREATE FUNCTION chapter_rollup_rebuild(p_sources uuid[])
RETURNS void
LANGUAGE plpgsql AS $$
BEGIN
  PERFORM 1 FROM chapter_rollup r
  WHERE r.series_source_id = ANY(p_sources)
  ORDER BY r.series_source_id, r.discovered_at
  FOR UPDATE;

  MERGE INTO chapter_rollup r
  USING (
    SELECT c.series_source_id, ss.provider_id,
           CASE WHEN c.discovered_at < now() - interval '7 days' THEN '-infinity'::timestamptz
                ELSE c.discovered_at END AS discovered_at,
           count(*)::int AS chapters,
           max(c.discovered_at) AS last_discovered_at
    FROM unnest(p_sources) AS s(id)
    JOIN series_sources ss ON ss.id = s.id
    JOIN chapters c ON c.series_source_id = s.id
    GROUP BY 1, 2, 3
  ) live
  ON r.series_source_id = live.series_source_id AND r.discovered_at = live.discovered_at
  WHEN MATCHED THEN UPDATE
    SET provider_id = live.provider_id, chapters = live.chapters,
        last_discovered_at = live.last_discovered_at
  WHEN NOT MATCHED THEN INSERT
    VALUES (live.series_source_id, live.provider_id, live.discovered_at, live.chapters,
            live.last_discovered_at)
  WHEN NOT MATCHED BY SOURCE AND r.series_source_id = ANY(p_sources) THEN DELETE;
END
$$;

-- ---- chapters ----------------------------------------------------------------------------

CREATE FUNCTION chapter_rollup_chapters_inserted() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  PERFORM chapter_rollup_apply(array_agg(src), array_agg(at), array_agg(n))
  FROM (SELECT series_source_id AS src, discovered_at AS at, count(*)::int AS n
        FROM new_rows GROUP BY 1, 2) d;
  RETURN NULL;
END
$$;

CREATE FUNCTION chapter_rollup_chapters_deleted() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  PERFORM chapter_rollup_apply(array_agg(src), array_agg(at), array_agg(n))
  FROM (SELECT series_source_id AS src, discovered_at AS at, -count(*)::int AS n
        FROM old_rows GROUP BY 1, 2) d;
  RETURN NULL;
END
$$;

CREATE FUNCTION chapter_rollup_chapters_updated() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  -- The ingest upsert rewrites titles, paths and access on every rescan; only a chapter that
  -- moved source or discovery instant changes a count, and the rest must fall through.
  PERFORM chapter_rollup_apply(array_agg(src), array_agg(at), array_agg(n))
  FROM (
    SELECT src, at, sum(n)::int AS n FROM (
      SELECT series_source_id AS src, discovered_at AS at, 1 AS n FROM (
        SELECT series_source_id, number_milli, discovered_at FROM new_rows
        EXCEPT ALL
        SELECT series_source_id, number_milli, discovered_at FROM old_rows) added
      UNION ALL
      SELECT series_source_id, discovered_at, -1 FROM (
        SELECT series_source_id, number_milli, discovered_at FROM old_rows
        EXCEPT ALL
        SELECT series_source_id, number_milli, discovered_at FROM new_rows) removed
    ) moved
    GROUP BY src, at
    HAVING sum(n) <> 0
  ) d;
  RETURN NULL;
END
$$;

CREATE FUNCTION chapter_rollup_chapters_truncated() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  DELETE FROM chapter_rollup;
  RETURN NULL;
END
$$;

CREATE TRIGGER chapter_rollup_chapters_insert AFTER INSERT ON chapters
  REFERENCING NEW TABLE AS new_rows
  FOR EACH STATEMENT EXECUTE FUNCTION chapter_rollup_chapters_inserted();
CREATE TRIGGER chapter_rollup_chapters_delete AFTER DELETE ON chapters
  REFERENCING OLD TABLE AS old_rows
  FOR EACH STATEMENT EXECUTE FUNCTION chapter_rollup_chapters_deleted();
CREATE TRIGGER chapter_rollup_chapters_update AFTER UPDATE ON chapters
  REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows
  FOR EACH STATEMENT EXECUTE FUNCTION chapter_rollup_chapters_updated();
CREATE TRIGGER chapter_rollup_chapters_truncate AFTER TRUNCATE ON chapters
  FOR EACH STATEMENT EXECUTE FUNCTION chapter_rollup_chapters_truncated();

-- ---- series_sources ----------------------------------------------------------------------

CREATE FUNCTION chapter_rollup_sources_updated() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  -- Every scan rewrites `chapter_count`/`last_scanned_at`; only a provider move is relevant.
  UPDATE chapter_rollup r SET provider_id = n.provider_id
  FROM old_rows o JOIN new_rows n ON n.id = o.id
  WHERE r.series_source_id = n.id AND o.provider_id IS DISTINCT FROM n.provider_id;
  RETURN NULL;
END
$$;

CREATE TRIGGER chapter_rollup_sources_update AFTER UPDATE ON series_sources
  REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows
  FOR EACH STATEMENT EXECUTE FUNCTION chapter_rollup_sources_updated();

-- ---- backfill ----------------------------------------------------------------------------

INSERT INTO chapter_rollup
  (series_source_id, provider_id, discovered_at, chapters, last_discovered_at)
SELECT c.series_source_id, ss.provider_id,
       CASE WHEN c.discovered_at < now() - interval '7 days' THEN '-infinity'::timestamptz
            ELSE c.discovered_at END,
       count(*)::int,
       max(c.discovered_at)
FROM chapters c
JOIN series_sources ss ON ss.id = c.series_source_id
GROUP BY 1, 2, 3;

ANALYZE chapter_rollup;
