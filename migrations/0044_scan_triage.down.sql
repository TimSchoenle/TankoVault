DROP INDEX IF EXISTS scan_runs_created_idx;
DROP INDEX IF EXISTS scan_runs_active;
DROP INDEX IF EXISTS scan_tasks_failed_recent;
ALTER TABLE scan_tasks DROP COLUMN IF EXISTS acknowledged_at;
