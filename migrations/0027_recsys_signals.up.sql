-- Recommendation signals, and the merge alias table (docs/RECOMMENDATIONS.md §5.1, §9.2).
--
-- Phase 0 of the suggestion system: everything downstream depends on this, and none of it is
-- user-visible on its own.

-- ---------------------------------------------------------------------------------------
-- pgvector
-- ---------------------------------------------------------------------------------------
-- A **hard dependency** of the deployment from here on, not an optional accelerator: the
-- recommender's retrieval path is an HNSW index over `series_embedding`, and there is no
-- fallback that works without it.
--
-- `deploy/docker-compose.yml` moves to `pgvector/pgvector:pg18` in the same change. An operator
-- running their own Postgres must install the extension before applying this migration; it
-- fails loudly rather than degrading, which is deliberate — a recommender that silently returns
-- nothing is worse than one that refuses to start.
CREATE EXTENSION IF NOT EXISTS vector;

-- ---------------------------------------------------------------------------------------
-- Tag vocabulary
-- ---------------------------------------------------------------------------------------
-- The catalogue's tags are AniList *genres* today: roughly twenty terms, of which "Action" and
-- "Fantasy" each cover a large fraction of the catalogue. A recommender on that vocabulary
-- cannot say anything specific. `kind` separates the coarse genre axis from AniList's ~600-term
-- descriptive tag vocabulary so the two can be weighted differently.
ALTER TABLE tags
  ADD COLUMN IF NOT EXISTS kind         text NOT NULL DEFAULT 'genre',
  ADD COLUMN IF NOT EXISTS series_count int  NOT NULL DEFAULT 0;

ALTER TABLE tags DROP CONSTRAINT IF EXISTS tags_kind_check;
ALTER TABLE tags
  ADD CONSTRAINT tags_kind_check
  CHECK (kind IN ('genre', 'theme', 'demographic', 'derived'));

-- `series_count` is a cached document frequency, written by the recommender's vocabulary pass.
-- It is a denormalisation on purpose: idf is read once per feature per build, and recomputing it
-- from `series_tags` each time is a full scan of the largest link table in the schema.

-- ---------------------------------------------------------------------------------------
-- Tag links gain a strength and a provenance
-- ---------------------------------------------------------------------------------------
-- `weight` is AniList's tag `rank`/100 where one exists, and 1.0 for a plain genre. Existing
-- rows are all genres, so the default backfills them correctly with no data migration.
ALTER TABLE series_tags
  ADD COLUMN IF NOT EXISTS weight real NOT NULL DEFAULT 1.0,
  ADD COLUMN IF NOT EXISTS source text NOT NULL DEFAULT 'provider';

ALTER TABLE series_tags DROP CONSTRAINT IF EXISTS series_tags_weight_check;
ALTER TABLE series_tags
  ADD CONSTRAINT series_tags_weight_check
  CHECK (weight > 0 AND weight <= 1);

-- ---------------------------------------------------------------------------------------
-- Series-level appeal signals and the adult gate
-- ---------------------------------------------------------------------------------------
-- All three are AniList-sourced and all three are nullable-by-absence rather than defaulted to
-- a fabricated value: a series nobody has scored must be distinguishable from one scored zero,
-- or the prior treats "unknown" as "bad".
ALTER TABLE series
  ADD COLUMN IF NOT EXISTS is_adult            boolean NOT NULL DEFAULT false,
  ADD COLUMN IF NOT EXISTS external_score      real,
  ADD COLUMN IF NOT EXISTS external_popularity int;

ALTER TABLE series DROP CONSTRAINT IF EXISTS series_external_score_check;
ALTER TABLE series
  ADD CONSTRAINT series_external_score_check
  CHECK (external_score IS NULL OR (external_score >= 0 AND external_score <= 100));

-- `is_adult` is a hard retrieval gate, never inferred from tags, and defaults to excluded.

-- ---------------------------------------------------------------------------------------
-- series_merges — where a merged series went
-- ---------------------------------------------------------------------------------------
-- `merge_series` deletes the absorbed row and, until now, left no forwarding record. With the
-- duplicate sweep performing automatic merges continuously, that makes every merged id a hard
-- 404 for bookmarks, shared links, external references and any client holding a stale id.
--
-- `merged_id` deliberately carries **no** foreign key: the row it names is gone, and that is
-- the entire point of the table.
CREATE TABLE IF NOT EXISTS series_merges (
  merged_id   uuid PRIMARY KEY,
  survivor_id uuid NOT NULL REFERENCES series(id) ON DELETE CASCADE,
  merged_at   timestamptz NOT NULL DEFAULT now(),
  merged_by   uuid REFERENCES users(id) ON DELETE SET NULL,
  CHECK (merged_id <> survivor_id)
);

-- Path compression is what keeps this map exactly one hop deep. When B is absorbed into C,
-- every row already pointing at B is re-pointed at C in the same transaction, so resolution is
-- a single lookup instead of a recursive walk that is both slower and able to spin on a cycle.
-- This index is what makes that `UPDATE ... WHERE survivor_id = $1` cheap; it is load-bearing
-- for the write path, not for reads.
CREATE INDEX IF NOT EXISTS series_merges_survivor_idx ON series_merges (survivor_id);
