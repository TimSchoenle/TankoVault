-- Coalesce a series' chapter notifications into one row while it stays unread.
--
-- Twelve chapters dropping overnight used to be twelve inbox rows and a bell reading `12`, none
-- of which said which series they were about. Grouping is what makes the count mean "things to
-- look at" rather than "events that happened".
ALTER TABLE notifications ADD COLUMN IF NOT EXISTS group_key text;

-- At most one *open* row per (user, group). This index is both the coalescing target for the
-- notifier's `ON CONFLICT` and the concurrency guard: two notifiers handling different chapters
-- of the same series serialise here instead of racing into two rows. Partial on `read_at IS NULL`
-- on purpose — once a reader has seen the row, the next chapter starts a fresh one, and the
-- history keeps as many rows as there were reading sessions.
CREATE UNIQUE INDEX IF NOT EXISTS notifications_open_group_idx
  ON notifications (user_id, group_key)
  WHERE read_at IS NULL AND group_key IS NOT NULL;

-- Existing rows keep a NULL `group_key` and stay ungrouped, which is what history should be:
-- they were written one per chapter and rewriting them would invent a reading session.
