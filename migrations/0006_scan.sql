-- Scan orchestration (progress + audit; mirrors the JetStream dispatch).
CREATE TABLE scan_runs (
  id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  provider_id  uuid REFERENCES providers(id) ON DELETE SET NULL,
  mode         scan_mode NOT NULL,
  state        run_state NOT NULL DEFAULT 'queued',
  total_tasks  int NOT NULL DEFAULT 0,
  done_tasks   int NOT NULL DEFAULT 0,
  failed_tasks int NOT NULL DEFAULT 0,
  started_at   timestamptz,
  finished_at  timestamptz,
  created_at   timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX scan_runs_provider_idx ON scan_runs (provider_id, created_at DESC);

CREATE TABLE scan_tasks (
  id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  run_id      uuid NOT NULL REFERENCES scan_runs(id) ON DELETE CASCADE,
  kind        text NOT NULL,                        -- 'catalog_page','series','latest_feed'
  target      jsonb NOT NULL,                       -- e.g. {"path":"/manga/x","page":3}
  state       task_state NOT NULL DEFAULT 'queued',
  attempts    smallint NOT NULL DEFAULT 0,
  worker_id   text,
  error       text,
  claimed_at  timestamptz,
  finished_at timestamptz
);
CREATE INDEX scan_tasks_run_state ON scan_tasks (run_id, state);
-- Durable claim path (fallback / audit): SELECT ... FOR UPDATE SKIP LOCKED.
CREATE INDEX scan_tasks_queue ON scan_tasks (state) WHERE state = 'queued';
