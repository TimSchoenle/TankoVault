-- Lock stored unread rows before recomputing them.
--
-- `refresh_watchlist_unread` (0058) computed the figures and wrote them in one statement, so the
-- snapshot it computed from was taken before the statement waited for the row lock. Two chapter
-- batches for one watched series, on two different sources, committing close together: the
-- second computes an unread count that is missing the first batch's chapters, waits for the row,
-- and overwrites the first batch's correct count with it. The reconciler eventually repairs the
-- row, and counts it as drift.
--
-- The rows are now locked first, in key order, and the figures computed in the next statement,
-- whose snapshot sees everything committed before the lock was granted. A row that does not exist
-- yet cannot be locked; two transactions creating the same row at once still race, and only a
-- watchlist entry created in the same instant as a chapter batch for it can reach that.

CREATE OR REPLACE FUNCTION refresh_watchlist_unread(p_user uuid, p_series uuid[])
RETURNS void
LANGUAGE plpgsql AS $$
BEGIN
  PERFORM 1 FROM watchlist_unread
  WHERE user_id = p_user AND series_id = ANY(p_series)
  ORDER BY series_id
  FOR UPDATE;

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
          EXCLUDED.next_unlock_at);
END
$$;
