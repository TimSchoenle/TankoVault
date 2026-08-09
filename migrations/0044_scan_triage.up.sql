-- Operator triage of the scan failure feed, and the indexes the live console reads through.

-- A cleared failure leaves the console's triage feed without leaving the history: the task row
-- keeps its `failed` state and its error, so the run counters, the audit trail and any later
-- investigation still see it. Clearing is a *view* decision, never a deletion — an operator
-- acknowledging noise must not be able to destroy the evidence of an outage.
ALTER TABLE scan_tasks ADD COLUMN acknowledged_at timestamptz;

-- The triage feed and its grouped twin both read `state = 'failed'` newest-first, and
-- `scan_tasks` grows by a row per series per scan. Without this the console's refresh
-- sequentially scans the whole table. `acknowledged_at` is INCLUDEd rather than added to the
-- predicate so the same index serves both the default feed (uncleared only) and the
-- show-cleared variant.
CREATE INDEX scan_tasks_failed_recent
  ON scan_tasks (finished_at DESC)
  INCLUDE (acknowledged_at)
  WHERE state = 'failed';

-- The live panel asks "which runs are in flight" every two seconds. `scan_runs_provider_idx`
-- is keyed on the provider, so an unfiltered question about state had nothing to read.
CREATE INDEX scan_runs_active
  ON scan_runs (created_at DESC)
  WHERE state IN ('queued', 'running');

-- The run history and the window summary both order and bound on `created_at` with no
-- provider named, which `scan_runs_provider_idx` cannot serve.
CREATE INDEX scan_runs_created_idx ON scan_runs (created_at DESC);
