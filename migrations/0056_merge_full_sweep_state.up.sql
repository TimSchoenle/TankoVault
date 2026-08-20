-- What the on-demand *exhaustive* duplicate sweep is doing, kept where an operator can read it.
--
-- The budgeted sweep answers its own request: one shortlist, a few hundred pairs, a report in
-- the response body. An exhaustive run cannot. It re-draws the shortlist until every pair the
-- rotations hold has been judged once, which takes far longer than the API's 25 s upstream
-- timeout — so it runs detached, the request that started it learns only that it started, and
-- this row is where it says what it has done since.
--
-- One row, like `metadata_sweep_state` and `rec_build_state`: what an operator needs is the
-- current or most recent run, not a history nobody prunes. The per-pair record already exists
-- in `merge_decisions`, which is where a run is read back pair by pair.
--
-- `claim_id` and `heartbeat_at` are the lease, and they are what makes `running` load-bearing
-- rather than advisory. Two exhaustive runs at once would spend the automatic-merge ceiling
-- twice over — the ceiling is the only bound on a background action that deletes series — so
-- the claim is real. And a run killed mid-way (a pod replaced, an OOM) would otherwise hold it
-- forever, which is the bug `rec_build_state` had to be given a lease in 0036 to fix. Every
-- write that advances or releases the claim carries the id it was granted, so a superseded
-- run's writes are no-ops instead of interleaving with the run that replaced it.
CREATE TABLE merge_full_sweep_state (
  id              boolean PRIMARY KEY DEFAULT true,
  running         boolean NOT NULL DEFAULT false,
  claim_id        uuid,
  heartbeat_at    timestamptz,
  started_at      timestamptz,
  finished_at     timestamptz,
  -- Rounds completed. One round is one budgeted sweep; the run keeps drawing rounds until one
  -- turns up no pair it has not already shortlisted.
  rounds          int NOT NULL DEFAULT 0,
  -- The counters `MergeSweepView` publishes, accumulated across every round so far. Written as
  -- the run advances, so the console shows progress rather than only a result.
  pairs_examined  bigint NOT NULL DEFAULT 0,
  auto_merged     bigint NOT NULL DEFAULT 0,
  queued          bigint NOT NULL DEFAULT 0,
  requeued        bigint NOT NULL DEFAULT 0,
  reopened        bigint NOT NULL DEFAULT 0,
  withdrawn       bigint NOT NULL DEFAULT 0,
  -- `distinct` is reserved; the column is the count of pairs judged apart.
  distinct_pairs  bigint NOT NULL DEFAULT 0,
  deferred        bigint NOT NULL DEFAULT 0,
  blocked         bigint NOT NULL DEFAULT 0,
  -- Why the last run stopped: 'exhausted', 'merge_ceiling', 'round_cap' or 'failed'. NULL while
  -- one is running. Only 'exhausted' means the catalogue was walked to the end; the other three
  -- mean the next run has more to do, which is a different sentence for the console to show.
  stopped         text,
  -- How the last run ended when it ended badly. A run that aborts still writes here and clears
  -- `running`, so a failed sweep is distinguishable from a stuck one.
  error           text,
  CHECK (id)
);
INSERT INTO merge_full_sweep_state (id) VALUES (true);
