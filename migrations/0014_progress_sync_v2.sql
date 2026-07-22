-- Reading Progress & External Sync v2 (docs/READING_PROGRESS_AND_SYNC.md).
--
-- Part A: split the single read-progress scalar into two independent frontiers so a
-- sub-chapter part release can never corrupt whole-chapter progress. The existing scalar is
-- renamed in place (its value IS the whole-chapter frontier) and a nullable part frontier is
-- added alongside it. No backfill: every user's part progress starts NULL.
ALTER TABLE read_progress
  RENAME COLUMN last_read_number TO last_read_whole_number;
ALTER TABLE read_progress
  ADD COLUMN last_read_part_number numeric(10,4);
-- Invariant (enforced in the repo layer at every write site, not a DB CHECK, since it
-- depends on floor(last_read_part_number) vs. last_read_whole_number):
--   last_read_part_number IS NULL OR floor(last_read_part_number) >= last_read_whole_number

-- Part A.5: per-series sync exclusion (opt-out / blacklist model). Every watchlisted series
-- is included in sync by default; a single toggle takes a title out.
ALTER TABLE watchlist_entries
  ADD COLUMN sync_excluded boolean NOT NULL DEFAULT false;

-- watchlist_entries needs its own change timestamp for newest_wins to compare status changes
-- fairly (added_at never changes after creation). Existing rows default to added_at's value.
ALTER TABLE watchlist_entries
  ADD COLUMN updated_at timestamptz NOT NULL DEFAULT now();

-- Optional per-provider override of the blanket sync_excluded flag, for users linking more
-- than one provider who want finer control.
CREATE TABLE series_sync_overrides (
  user_id   uuid NOT NULL REFERENCES users(id)  ON DELETE CASCADE,
  series_id uuid NOT NULL REFERENCES series(id) ON DELETE CASCADE,
  provider  text NOT NULL,
  excluded  boolean NOT NULL,
  PRIMARY KEY (user_id, series_id, provider)
);

-- Part B.2: per-account automatic-sync policy, persisted (replaces the env-only default).
ALTER TABLE external_accounts
  ADD COLUMN auto_sync_enabled boolean NOT NULL DEFAULT true,
  ADD COLUMN conflict_policy   text    NOT NULL DEFAULT 'newest_wins';

-- The three-way merge "common ancestor": what both sides agreed on as of the last successful
-- reconciliation, so the engine can tell which side(s) actually changed since.
ALTER TABLE sync_mappings
  ADD COLUMN last_synced_local_progress  double precision,
  ADD COLUMN last_synced_remote_progress double precision,
  ADD COLUMN last_synced_local_status    text,
  ADD COLUMN last_synced_remote_status   text,
  ADD COLUMN last_synced_at              timestamptz;

-- A genuine, unresolved conflict awaiting user input under the 'ask_me' policy.
CREATE TABLE sync_conflicts (
  id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  user_id      uuid NOT NULL REFERENCES users(id)  ON DELETE CASCADE,
  series_id    uuid NOT NULL REFERENCES series(id) ON DELETE CASCADE,
  provider     text NOT NULL,
  field        text NOT NULL,             -- 'progress' | 'status'
  local_value  text NOT NULL,
  remote_value text NOT NULL,
  detected_at  timestamptz NOT NULL DEFAULT now(),
  resolved_at  timestamptz,
  resolution   text                       -- 'local' | 'remote', NULL while pending
);
CREATE INDEX sync_conflicts_pending_idx ON sync_conflicts (user_id) WHERE resolved_at IS NULL;
-- One pending conflict per (user, series, provider, field): idempotent re-detection.
CREATE UNIQUE INDEX sync_conflicts_unique_pending_idx
  ON sync_conflicts (user_id, series_id, provider, field)
  WHERE resolved_at IS NULL;

-- User-facing sync history (distinct from the operator-facing audit_log): what the automatic
-- engine actually did, so "automatic" never means "invisible."
CREATE TABLE sync_history (
  id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  user_id    uuid NOT NULL REFERENCES users(id)  ON DELETE CASCADE,
  series_id  uuid NOT NULL REFERENCES series(id) ON DELETE CASCADE,
  provider   text NOT NULL,
  action     text NOT NULL,    -- 'push' | 'pull' | 'conflict_auto' | 'conflict_manual'
  detail     jsonb NOT NULL,
  created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX sync_history_user_idx ON sync_history (user_id, created_at DESC);
