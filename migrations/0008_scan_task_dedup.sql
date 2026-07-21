-- Idempotent scan-task fan-out (design §12).
--
-- Under JetStream's at-least-once delivery a `catalog_page` task can be redelivered (e.g.
-- its processing outran the ack deadline, or a worker crashed mid-fan-out). Re-processing
-- it must not re-enqueue a fresh `series` task per catalogue entry — that multiplies the
-- crawl load on the provider. Deduping child tasks on (run_id, kind, target) lets the
-- fan-out use INSERT ... ON CONFLICT DO NOTHING, so a redelivered page is a cheap no-op.
--
-- `target` is small canonical JSON (`{"page":2}` / `{"path":"/manga/x"}`); its jsonb text
-- form is a stable per-value key, so the expression index gives exact-target uniqueness.
CREATE UNIQUE INDEX scan_tasks_run_kind_target
    ON scan_tasks (run_id, kind, (target::text));
