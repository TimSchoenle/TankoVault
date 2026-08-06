-- What the tokenless metadata-enrichment sweep did, kept where an operator can read it.
--
-- The sweep already reported itself: one `tracing::info!` line per run, carrying scanned /
-- enriched / unresolved. That is invisible to anyone without shell access to the sync
-- container's logs, and it is gone the moment the log window rolls — so "is AniList enrichment
-- working?" had no answer short of diffing the catalogue. Worse, the two failure modes look
-- identical from outside: a sweep that ran and resolved nothing, and a sweep that never ran at
-- all (no public-metadata provider registered, or `metadata.enrich_enabled = false`), both
-- leave the catalogue exactly as it was.
--
-- One row, like `rec_build_state`: the sweep is a per-replica loop over a shared catalogue, and
-- what an operator needs is the latest outcome, not a history nobody prunes. `running` is
-- advisory only — it says a sweep is in flight so the console can show progress, and is *not* a
-- claim: unlike a recsys build the sweep is idempotent per series and safe to overlap.
CREATE TABLE metadata_sweep_state (
  id          boolean PRIMARY KEY DEFAULT true,
  running     boolean NOT NULL DEFAULT false,
  started_at  timestamptz,
  finished_at timestamptz,
  -- Series examined by the current or most recent sweep. Written as the sweep advances, so the
  -- console can show progress rather than only a result.
  scanned     int NOT NULL DEFAULT 0,
  enriched    int NOT NULL DEFAULT 0,
  unresolved  int NOT NULL DEFAULT 0,
  -- How the last sweep ended when it ended badly. A run that aborts mid-way still writes here
  -- and clears `running`, so a stuck sweep is distinguishable from a failed one.
  error       text,
  CHECK (id)
);
INSERT INTO metadata_sweep_state (id) VALUES (true);
