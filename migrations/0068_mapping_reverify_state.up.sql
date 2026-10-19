-- The operator's re-verification of stored `AniList` mappings: whether one is running and what
-- the last one found.
--
-- Enrichment once wrote a mapping for whatever a title search ranked first, including a light
-- novel sharing the manga's title. Re-verification re-fetches every mapping by id and refuses
-- the ones whose record is not a library format or whose titles do not identify the series. It
-- paces itself against `AniList`'s rate limit, so it runs for hours and outlives any request;
-- this row is how the operator follows it.
--
-- Unlike `metadata_sweep_state`, `running` is a claim: two runs would each spend the rate budget
-- on the same mappings. `heartbeat_at` lets a claim abandoned by a crashed replica be retaken.
CREATE TABLE mapping_reverify_state (
  id           boolean PRIMARY KEY DEFAULT true,
  running      boolean NOT NULL DEFAULT false,
  dry_run      boolean NOT NULL DEFAULT true,
  started_at   timestamptz,
  heartbeat_at timestamptz,
  finished_at  timestamptz,
  checked      int NOT NULL DEFAULT 0,
  failed       int NOT NULL DEFAULT 0,
  removed      int NOT NULL DEFAULT 0,
  unreachable  int NOT NULL DEFAULT 0,
  -- The failing mappings of the last run, capped; a dry run is reviewed from this list.
  failures     jsonb NOT NULL DEFAULT '[]'::jsonb,
  error        text,
  CHECK (id)
);
INSERT INTO mapping_reverify_state (id) VALUES (true);
