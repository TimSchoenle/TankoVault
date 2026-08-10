-- Stage telemetry for scan tasks: what a task is doing right now, and where its wall clock went.
--
-- `scan_tasks.state` distinguishes queued from claimed from done. That counts a run; it cannot
-- explain one. A task claimed twenty minutes ago and a task wedged twenty minutes ago are the
-- same row through `state`, and the operator console had no way to tell them apart — which is
-- exactly the question a scan that has been "running" for twenty minutes raises.

-- When the task row was created, so the console can separate the time a task spent waiting for a
-- worker from the time it spent working. A worker serves at most one task per provider, so a
-- provider's second run legitimately sits queued behind its first; without this the wait reads as
-- execution and the run looks stuck.
--
-- Deliberately nullable with the default added separately rather than `NOT NULL DEFAULT now()`:
-- `now()` is volatile, so a default on ADD COLUMN rewrites the whole table under an ACCESS
-- EXCLUSIVE lock, and this table carries a row per series per scan. Rows written before this
-- migration keep NULL, which is the honest answer — their creation time was never recorded.
ALTER TABLE scan_tasks ADD COLUMN created_at timestamptz;
ALTER TABLE scan_tasks ALTER COLUMN created_at SET DEFAULT now();

-- The live stage: which of the half-dozen things a task does it is doing, since when, how far
-- through, and against what.
--
-- `text` rather than a Postgres enum, because a stage is a diagnostic label: adding one must not
-- need a migration and an ACCESS EXCLUSIVE lock on this table. The vocabulary is
-- `tankovault_domain::ScanStage`, which round-trips its tokens in a unit test.
ALTER TABLE scan_tasks ADD COLUMN stage text;
ALTER TABLE scan_tasks ADD COLUMN stage_at timestamptz;
ALTER TABLE scan_tasks ADD COLUMN stage_done int;
ALTER TABLE scan_tasks ADD COLUMN stage_total int;
ALTER TABLE scan_tasks ADD COLUMN stage_detail text;

-- The settled breakdown, written once when the task ends.
--
-- `wait_ms` is queue time, `duration_ms` is execution time, and `telemetry` carries the per-stage
-- milliseconds plus what the fetch stack spent them on — requests, time inside a request, time
-- waiting for permission to send one, challenge solves, throttling responses. The pace-wait
-- figure is the one that answers the operator's actual question: a scan at 95% pace-wait is
-- polite, not broken, and nothing in the code will make it faster.
ALTER TABLE scan_tasks ADD COLUMN wait_ms int;
ALTER TABLE scan_tasks ADD COLUMN duration_ms int;
ALTER TABLE scan_tasks ADD COLUMN telemetry jsonb;

-- The run drawer reads one run's tasks slowest-first to answer "which of these is costing me the
-- time". Without this it sorts the run's whole task set — up to one row per series — in memory.
CREATE INDEX scan_tasks_run_duration ON scan_tasks (run_id, duration_ms DESC NULLS LAST);
