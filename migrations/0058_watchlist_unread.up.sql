-- Stored unread state per (reader, watched series), kept true by triggers.
--
-- Design and measurements: docs/perf/UNREAD_DENORMALISATION.md. Every Home and watchlist read used
-- to re-derive these figures from `chapters` for the whole watchlist on every request (6–30 s in
-- production). They are now computed when an input changes and read by primary key.
--
-- ---------------------------------------------------------------------------------------
-- The invariant
-- ---------------------------------------------------------------------------------------
-- A row here equals `watchlist_unread_live(user, series)` for its key, except for chapters whose
-- `unlocks_at` has passed since the row was computed; `next_unlock_at` names the earliest such
-- moment and the control plane's sweeper recomputes rows once it arrives. The reconciler in
-- `repo::tracking::unread` re-verifies rows against the live function and counts any drift.
--
-- `watchlist_unread_live` is the single SQL spelling the stored values come from. Its unread
-- predicate is the one `repo/tracking/dashboard.rs` documents, clause for clause: `floor()` before
-- `::bigint`, `bigint` rather than `int`, and the early-access opt-in as an uncorrelated
-- `ANY(ARRAY(SELECT … WHERE e.user_id = p_user))` InitPlan.
--
-- ---------------------------------------------------------------------------------------
-- The triggers, and why one per event
-- ---------------------------------------------------------------------------------------
-- Postgres refuses transition tables on a trigger with more than one event or with an `UPDATE OF`
-- column list, so every table has one statement-level trigger per event, and each `UPDATE`
-- trigger finds the rows that matter by comparing its old and new transition tables itself. That
-- comparison is load-bearing: the ingest upsert rewrites titles and paths, and every scan rewrites
-- `series_sources.chapter_count`; neither changes a stored value, and neither may recompute one.
--
-- `series_sources` has no INSERT trigger: a source is created before any chapter can reference
-- it, so an inserted source changes nothing a row stores. Its chapters arrive through the
-- `chapters` INSERT trigger. `watchlist_entries` has no DELETE trigger: the stored row cascades.
--
-- Keys are always refreshed in (user_id, series_id) order, so a chapter batch and a reader's
-- progress write that touch the same rows queue behind each other instead of deadlocking.
--
-- ---------------------------------------------------------------------------------------
-- Lock impact
-- ---------------------------------------------------------------------------------------
-- `CREATE TRIGGER` takes SHARE ROW EXCLUSIVE on `chapters`, `series_sources`, `read_progress`,
-- `watchlist_entries` and `user_provider_early_access`, held until this migration commits,
-- backfill included. Reads continue; ingest, progress writes and watchlist changes wait. The
-- backfill costs ~0.19 ms per watchlist row warm on a 2.84 M-chapter catalogue, so 5 000 rows
-- hold writers for about a second, several seconds on a cold cache.

CREATE TABLE watchlist_unread (
  user_id            uuid NOT NULL,
  series_id          uuid NOT NULL,
  -- Distinct whole chapters the reader has not read and can open.
  unread_count       int  NOT NULL,
  -- The lowest such chapter, as `number_milli`.
  next_unread_milli  int,
  -- Distinct whole chapters the reader can open.
  total_chapters     int  NOT NULL,
  -- Of those, at or below the whole frontier.
  read_count         int  NOT NULL,
  -- The highest chapter the reader can open.
  latest_milli       int,
  -- The newest sighting among chapters the reader can open.
  latest_readable_at timestamptz,
  -- When a chapter this reader cannot open yet next becomes readable by time.
  next_unlock_at     timestamptz,
  -- When the reconciler last compared this row against the live function.
  verified_at        timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (user_id, series_id),
  FOREIGN KEY (user_id, series_id) REFERENCES watchlist_entries (user_id, series_id)
    ON DELETE CASCADE
);

CREATE INDEX watchlist_unread_unlock ON watchlist_unread (next_unlock_at)
  WHERE next_unlock_at IS NOT NULL;
CREATE INDEX watchlist_unread_verified ON watchlist_unread (verified_at);

CREATE FUNCTION watchlist_unread_live(p_user uuid, p_series uuid[])
RETURNS TABLE (
  user_id            uuid,
  series_id          uuid,
  unread_count       int,
  next_unread_milli  int,
  total_chapters     int,
  read_count         int,
  latest_milli       int,
  latest_readable_at timestamptz,
  next_unlock_at     timestamptz
)
LANGUAGE sql STABLE AS $$
  SELECT w.user_id, w.series_id,
         COALESCE(unr.unread, 0)::int,
         unr.next_milli,
         COALESCE(tot.total_chapters, 0)::int,
         COALESCE(tot.read_count, 0)::int,
         tot.latest_milli,
         act.latest_readable_at,
         lck.next_unlock_at
  FROM watchlist_entries w
  LEFT JOIN read_progress rp ON rp.user_id = w.user_id AND rp.series_id = w.series_id
  CROSS JOIN LATERAL (
    SELECT count(DISTINCT c.number_milli / 10000) AS unread, min(c.number_milli) AS next_milli
    FROM series_sources ss JOIN chapters c ON c.series_source_id = ss.id
    WHERE ss.series_id = w.series_id
      AND c.number_milli >= (floor(COALESCE(rp.last_read_whole_number, 0))::bigint + 1) * 10000
      AND NOT (c.number_milli % 10000 <> 0
               AND rp.last_read_part_number IS NOT NULL
               AND c.number_milli <= (rp.last_read_part_number * 10000)::bigint)
      AND (c.access = 'free' OR c.unlocks_at <= now()
           OR ss.provider_id = ANY(ARRAY(
                SELECT e.provider_id FROM user_provider_early_access e
                WHERE e.user_id = p_user)))
  ) unr
  CROSS JOIN LATERAL (
    SELECT count(DISTINCT c.number_milli / 10000) AS total_chapters,
           count(DISTINCT c.number_milli / 10000) FILTER (
             WHERE c.number_milli < (floor(COALESCE(rp.last_read_whole_number, 0))::bigint + 1) * 10000
           ) AS read_count,
           max(c.number_milli) AS latest_milli
    FROM series_sources ss JOIN chapters c ON c.series_source_id = ss.id
    WHERE ss.series_id = w.series_id
      AND (c.access = 'free' OR c.unlocks_at <= now()
           OR ss.provider_id = ANY(ARRAY(
                SELECT e.provider_id FROM user_provider_early_access e
                WHERE e.user_id = p_user)))
  ) tot
  CROSS JOIN LATERAL (
    -- Scalar max per source: the form the MIN/MAX optimisation turns into one backward probe of
    -- `chapters_source_disc_access_idx`.
    SELECT max((SELECT max(c.discovered_at) FROM chapters c
                WHERE c.series_source_id = ss.id
                  AND (c.access = 'free' OR c.unlocks_at <= now()
                       OR ss.provider_id = ANY(ARRAY(
                            SELECT e.provider_id FROM user_provider_early_access e
                            WHERE e.user_id = p_user))))) AS latest_readable_at
    FROM series_sources ss WHERE ss.series_id = w.series_id
  ) act
  CROSS JOIN LATERAL (
    SELECT min(c.unlocks_at) AS next_unlock_at
    FROM series_sources ss JOIN chapters c ON c.series_source_id = ss.id
    WHERE ss.series_id = w.series_id
      AND c.access <> 'free' AND c.unlocks_at > now()
      AND NOT ss.provider_id = ANY(ARRAY(
                SELECT e.provider_id FROM user_provider_early_access e
                WHERE e.user_id = p_user))
  ) lck
  WHERE w.user_id = p_user AND w.series_id = ANY(p_series)
$$;

-- Upsert the stored rows for one reader's `p_series` from the live function. A row whose values
-- did not change is not rewritten.
CREATE FUNCTION refresh_watchlist_unread(p_user uuid, p_series uuid[])
RETURNS void
LANGUAGE sql AS $$
  INSERT INTO watchlist_unread AS u
         (user_id, series_id, unread_count, next_unread_milli, total_chapters, read_count,
          latest_milli, latest_readable_at, next_unlock_at)
  SELECT l.user_id, l.series_id, l.unread_count, l.next_unread_milli, l.total_chapters,
         l.read_count, l.latest_milli, l.latest_readable_at, l.next_unlock_at
  FROM watchlist_unread_live(p_user, p_series) l
  ORDER BY l.series_id
  ON CONFLICT (user_id, series_id) DO UPDATE
     SET unread_count       = EXCLUDED.unread_count,
         next_unread_milli  = EXCLUDED.next_unread_milli,
         total_chapters     = EXCLUDED.total_chapters,
         read_count         = EXCLUDED.read_count,
         latest_milli       = EXCLUDED.latest_milli,
         latest_readable_at = EXCLUDED.latest_readable_at,
         next_unlock_at     = EXCLUDED.next_unlock_at
   WHERE (u.unread_count, u.next_unread_milli, u.total_chapters, u.read_count, u.latest_milli,
          u.latest_readable_at, u.next_unlock_at)
         IS DISTINCT FROM
         (EXCLUDED.unread_count, EXCLUDED.next_unread_milli, EXCLUDED.total_chapters,
          EXCLUDED.read_count, EXCLUDED.latest_milli, EXCLUDED.latest_readable_at,
          EXCLUDED.next_unlock_at)
$$;

-- Refresh every watcher of `p_series`, one reader at a time, in key order.
CREATE FUNCTION refresh_watchlist_unread_for_series(p_series uuid[])
RETURNS void
LANGUAGE plpgsql AS $$
DECLARE
  r record;
BEGIN
  FOR r IN
    SELECT w.user_id, array_agg(w.series_id ORDER BY w.series_id) AS series
    FROM watchlist_entries w
    WHERE w.series_id = ANY(p_series)
    GROUP BY w.user_id
    ORDER BY w.user_id
  LOOP
    PERFORM refresh_watchlist_unread(r.user_id, r.series);
  END LOOP;
END
$$;

-- ---- chapters ----------------------------------------------------------------------------

CREATE FUNCTION watchlist_unread_chapters_inserted() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  PERFORM refresh_watchlist_unread_for_series(ARRAY(
    SELECT DISTINCT ss.series_id
    FROM (SELECT DISTINCT series_source_id FROM new_rows) c
    JOIN series_sources ss ON ss.id = c.series_source_id));
  RETURN NULL;
END
$$;

CREATE FUNCTION watchlist_unread_chapters_deleted() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  -- A cascade from a deleted source finds no source here; the source's own trigger covers it.
  PERFORM refresh_watchlist_unread_for_series(ARRAY(
    SELECT DISTINCT ss.series_id
    FROM (SELECT DISTINCT series_source_id FROM old_rows) c
    JOIN series_sources ss ON ss.id = c.series_source_id));
  RETURN NULL;
END
$$;

CREATE FUNCTION watchlist_unread_chapters_updated() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  PERFORM refresh_watchlist_unread_for_series(ARRAY(
    SELECT DISTINCT ss.series_id
    FROM (
      (SELECT series_source_id, number_milli, access, unlocks_at, discovered_at FROM new_rows
       EXCEPT
       SELECT series_source_id, number_milli, access, unlocks_at, discovered_at FROM old_rows)
      UNION
      (SELECT series_source_id, number_milli, access, unlocks_at, discovered_at FROM old_rows
       EXCEPT
       SELECT series_source_id, number_milli, access, unlocks_at, discovered_at FROM new_rows)
    ) c
    JOIN series_sources ss ON ss.id = c.series_source_id));
  RETURN NULL;
END
$$;

CREATE TRIGGER watchlist_unread_chapters_insert AFTER INSERT ON chapters
  REFERENCING NEW TABLE AS new_rows
  FOR EACH STATEMENT EXECUTE FUNCTION watchlist_unread_chapters_inserted();
CREATE TRIGGER watchlist_unread_chapters_delete AFTER DELETE ON chapters
  REFERENCING OLD TABLE AS old_rows
  FOR EACH STATEMENT EXECUTE FUNCTION watchlist_unread_chapters_deleted();
CREATE TRIGGER watchlist_unread_chapters_update AFTER UPDATE ON chapters
  REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows
  FOR EACH STATEMENT EXECUTE FUNCTION watchlist_unread_chapters_updated();

-- ---- series_sources ----------------------------------------------------------------------

CREATE FUNCTION watchlist_unread_sources_deleted() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  PERFORM refresh_watchlist_unread_for_series(ARRAY(SELECT DISTINCT series_id FROM old_rows));
  RETURN NULL;
END
$$;

CREATE FUNCTION watchlist_unread_sources_updated() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  -- Only a source moving between series or providers changes what a reader can count; the
  -- per-scan `chapter_count`/`last_scanned_at` rewrite must fall through with nothing to do.
  PERFORM refresh_watchlist_unread_for_series(ARRAY(
    SELECT DISTINCT v.series_id FROM (
      SELECT o.series_id AS from_series, n.series_id AS to_series
      FROM old_rows o JOIN new_rows n ON n.id = o.id
      WHERE (o.series_id, o.provider_id) IS DISTINCT FROM (n.series_id, n.provider_id)
    ) moved, LATERAL (VALUES (moved.from_series), (moved.to_series)) AS v(series_id)));
  RETURN NULL;
END
$$;

CREATE TRIGGER watchlist_unread_sources_delete AFTER DELETE ON series_sources
  REFERENCING OLD TABLE AS old_rows
  FOR EACH STATEMENT EXECUTE FUNCTION watchlist_unread_sources_deleted();
CREATE TRIGGER watchlist_unread_sources_update AFTER UPDATE ON series_sources
  REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows
  FOR EACH STATEMENT EXECUTE FUNCTION watchlist_unread_sources_updated();

-- ---- read_progress and watchlist_entries -------------------------------------------------

-- Refresh the (user_id, series_id) keys a transition table names. Shared by the progress and
-- watchlist triggers, which differ only in which table they read the keys from.
CREATE FUNCTION watchlist_unread_refresh_keys(p_keys jsonb) RETURNS void
LANGUAGE plpgsql AS $$
DECLARE
  r record;
BEGIN
  FOR r IN
    SELECT k.user_id, array_agg(DISTINCT k.series_id ORDER BY k.series_id) AS series
    FROM jsonb_to_recordset(p_keys) AS k(user_id uuid, series_id uuid)
    GROUP BY k.user_id
    ORDER BY k.user_id
  LOOP
    PERFORM refresh_watchlist_unread(r.user_id, r.series);
  END LOOP;
END
$$;

CREATE FUNCTION watchlist_unread_progress_new() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  PERFORM watchlist_unread_refresh_keys(
    (SELECT COALESCE(jsonb_agg(jsonb_build_object('user_id', user_id, 'series_id', series_id)),
                     '[]') FROM new_rows));
  RETURN NULL;
END
$$;

CREATE FUNCTION watchlist_unread_progress_old() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  PERFORM watchlist_unread_refresh_keys(
    (SELECT COALESCE(jsonb_agg(jsonb_build_object('user_id', user_id, 'series_id', series_id)),
                     '[]') FROM old_rows));
  RETURN NULL;
END
$$;

-- An update can move a row to another key, so both halves are refreshed.
CREATE FUNCTION watchlist_unread_progress_moved() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  PERFORM watchlist_unread_refresh_keys(
    (SELECT COALESCE(jsonb_agg(jsonb_build_object('user_id', k.user_id, 'series_id', k.series_id)),
                     '[]')
     FROM (SELECT user_id, series_id FROM new_rows
           UNION SELECT user_id, series_id FROM old_rows) k));
  RETURN NULL;
END
$$;

CREATE TRIGGER watchlist_unread_progress_insert AFTER INSERT ON read_progress
  REFERENCING NEW TABLE AS new_rows
  FOR EACH STATEMENT EXECUTE FUNCTION watchlist_unread_progress_new();
CREATE TRIGGER watchlist_unread_progress_update AFTER UPDATE ON read_progress
  REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows
  FOR EACH STATEMENT EXECUTE FUNCTION watchlist_unread_progress_moved();
CREATE TRIGGER watchlist_unread_progress_delete AFTER DELETE ON read_progress
  REFERENCING OLD TABLE AS old_rows
  FOR EACH STATEMENT EXECUTE FUNCTION watchlist_unread_progress_old();
CREATE TRIGGER watchlist_unread_watchlist_insert AFTER INSERT ON watchlist_entries
  REFERENCING NEW TABLE AS new_rows
  FOR EACH STATEMENT EXECUTE FUNCTION watchlist_unread_progress_new();

-- ---- user_provider_early_access ----------------------------------------------------------

CREATE FUNCTION watchlist_unread_early_access_changed() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
  r record;
BEGIN
  FOR r IN
    SELECT w.user_id, array_agg(DISTINCT w.series_id ORDER BY w.series_id) AS series
    FROM changed t
    JOIN watchlist_entries w ON w.user_id = t.user_id
    JOIN series_sources ss ON ss.series_id = w.series_id AND ss.provider_id = t.provider_id
    GROUP BY w.user_id
    ORDER BY w.user_id
  LOOP
    PERFORM refresh_watchlist_unread(r.user_id, r.series);
  END LOOP;
  RETURN NULL;
END
$$;

CREATE TRIGGER watchlist_unread_early_access_insert AFTER INSERT ON user_provider_early_access
  REFERENCING NEW TABLE AS changed
  FOR EACH STATEMENT EXECUTE FUNCTION watchlist_unread_early_access_changed();
CREATE TRIGGER watchlist_unread_early_access_delete AFTER DELETE ON user_provider_early_access
  REFERENCING OLD TABLE AS changed
  FOR EACH STATEMENT EXECUTE FUNCTION watchlist_unread_early_access_changed();

-- ---- backfill ----------------------------------------------------------------------------

INSERT INTO watchlist_unread
       (user_id, series_id, unread_count, next_unread_milli, total_chapters, read_count,
        latest_milli, latest_readable_at, next_unlock_at)
SELECT l.user_id, l.series_id, l.unread_count, l.next_unread_milli, l.total_chapters,
       l.read_count, l.latest_milli, l.latest_readable_at, l.next_unlock_at
FROM (SELECT user_id, array_agg(series_id ORDER BY series_id) AS series
      FROM watchlist_entries GROUP BY user_id) w
CROSS JOIN LATERAL watchlist_unread_live(w.user_id, w.series) l
ORDER BY l.user_id, l.series_id;
