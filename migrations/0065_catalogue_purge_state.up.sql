-- What the catalogue purge is doing, kept where an operator can read it.
--
-- The purge used to be a loop of HTTP calls, each deleting batches for up to ten seconds. One
-- batch alone could outlast the API's 30 s request timeout on a large catalogue, the handler
-- future was dropped mid-transaction, the batch rolled back, and the purge made no progress at
-- all. It now runs detached: the request that starts it learns only that it started, and this
-- row is where the run reports what it has removed since.
--
-- One row, like `merge_full_sweep_state`. `claim_id` and `heartbeat_at` are the lease: two runs
-- at once would contend for the same rows, and a run killed with its process would otherwise
-- hold the claim forever. Every write that advances or releases the claim carries the id it was
-- granted, so a superseded run's writes are no-ops.
CREATE TABLE catalogue_purge_state (
  id                boolean PRIMARY KEY DEFAULT true,
  running           boolean NOT NULL DEFAULT false,
  claim_id          uuid,
  heartbeat_at      timestamptz,
  started_at        timestamptz,
  finished_at       timestamptz,
  -- 'chapters' or 'everything'. NULL before the first run.
  scope             text,
  -- Who started the current or last run. Not a foreign key: the row must outlive an erased
  -- operator, and the audit log is the authoritative record of who did it.
  started_by        uuid,
  -- Set by the cancel route, read by the run after every batch.
  cancel_requested  boolean NOT NULL DEFAULT false,
  series_removed    bigint NOT NULL DEFAULT 0,
  sources_removed   bigint NOT NULL DEFAULT 0,
  chapters_removed  bigint NOT NULL DEFAULT 0,
  watchlist_removed bigint NOT NULL DEFAULT 0,
  progress_removed  bigint NOT NULL DEFAULT 0,
  -- Rows of the purged kind still standing after the last committed batch.
  remaining         bigint,
  -- 'done', 'cancelled', 'stalled' or 'failed'. NULL while running, and for a run whose
  -- process died before it could say.
  stopped           text,
  error             text,
  CHECK (id)
);
INSERT INTO catalogue_purge_state (id) VALUES (true);
