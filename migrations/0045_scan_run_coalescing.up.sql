-- Index for the planner's coalescing guard: "does this provider already have a run of this mode
-- in flight?", asked once per provider per scheduler tick.
--
-- Neither existing index answers it. `scan_runs_provider_idx` is keyed on the provider but spans
-- every state, so it walks the provider's whole history; `scan_runs_active` is restricted to the
-- in-flight states but keyed on `created_at` alone, so it walks every provider's active runs.
CREATE INDEX scan_runs_active_provider_mode
  ON scan_runs (provider_id, mode, created_at DESC)
  WHERE state IN ('queued', 'running');
