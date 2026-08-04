-- The recommender's item model (docs/RECOMMENDATIONS.md §5.2).
--
-- Every table here is derived: it can be dropped and rebuilt from `series` and its link tables
-- with no loss. That is what makes `generation` sufficient for an atomic-ish rebuild, and why
-- none of it is exported for GDPR — it holds no personal data.
--
-- Every table is keyed by `series_id` with a cascading foreign key, and that is the whole merge
-- story for the item model: an absorbed series' rows disappear in the same transaction that
-- deletes it, so no query can return a merged id afterwards.

-- ---------------------------------------------------------------------------------------
-- Feature vocabulary
-- ---------------------------------------------------------------------------------------
-- Interned, so a per-series vector stores `int4` ids rather than repeating strings. `kind` is
-- part of the key because "Action" the tag and "Action" the hypothetical author are different
-- features that must not collide.
CREATE TABLE rec_features (
  id        int GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  kind      text NOT NULL,
  value     text NOT NULL,
  doc_count int  NOT NULL DEFAULT 0,
  idf       real NOT NULL DEFAULT 0,
  UNIQUE (kind, value)
);

ALTER TABLE rec_features
  ADD CONSTRAINT rec_features_kind_check
  CHECK (kind IN ('tag', 'author', 'content_type', 'country', 'status', 'decade', 'length'));

-- The ordinal used as the SVD input index, assigned to the most frequent non-author features
-- only. NULL means "scores and explains, but does not shape the embedding" — authors always,
-- and any feature past the input cap.
--
-- Held here rather than recomputed per build because the embedding of an *unchanged* series
-- must not move between incremental builds: the column is what pins the basis' column order.
ALTER TABLE rec_features ADD COLUMN dense_index int;
CREATE UNIQUE INDEX rec_features_dense_idx ON rec_features (dense_index)
  WHERE dense_index IS NOT NULL;

-- ---------------------------------------------------------------------------------------
-- Sparse vectors
-- ---------------------------------------------------------------------------------------
-- One row per series: the L2-normalised TF-IDF vector, feature ids ascending.
--
-- Parallel arrays, not a row per (series, feature): this is read only by primary key — to score
-- a candidate and to explain a pair — and never joined on the feature. The array form keeps that
-- one page instead of twenty index tuples. The ids are *feature* ids, so nothing here can dangle
-- when a series is merged away.
CREATE TABLE series_features (
  series_id   uuid PRIMARY KEY REFERENCES series(id) ON DELETE CASCADE,
  feature_ids int[]  NOT NULL,
  weights     real[] NOT NULL,
  digest      bytea  NOT NULL,
  generation  int    NOT NULL,
  built_at    timestamptz NOT NULL DEFAULT now(),
  CHECK (cardinality(feature_ids) = cardinality(weights))
);

-- The incremental build's work list: series whose inputs changed since they were last extracted.
CREATE INDEX series_features_generation_idx ON series_features (generation);

-- ---------------------------------------------------------------------------------------
-- The dense space
-- ---------------------------------------------------------------------------------------
-- `halfvec` (fp16), not `vector` (fp32): 128 dims x 2 bytes = 256 B per row, so the table is
-- ~256 MB at a million series and the HNSW graph stays cacheable. The precision loss is far
-- below what a ranking can resolve, and it is the difference between an index that lives in
-- memory and one that does not.
CREATE TABLE series_embedding (
  series_id  uuid PRIMARY KEY REFERENCES series(id) ON DELETE CASCADE,
  embedding  halfvec(128) NOT NULL,
  generation int NOT NULL,
  built_at   timestamptz NOT NULL DEFAULT now()
);

-- **The HNSW index is created by the builder, not here.**
--
-- Not an oversight: `CREATE INDEX CONCURRENTLY` cannot run inside the migrator's implicit
-- transaction (migration 0020 documents that trap at length), and a blocking HNSW build over a
-- million rows is minutes of ACCESS EXCLUSIVE on a table the API reads on every request. The
-- builder owns it, where it can be built concurrently, rebuilt on a dimension change, and
-- reported on. A deployment that has never run a build has no index and no rows, which is the
-- same thing as far as any reader is concerned.
--
-- The empty-table case is why this is safe to defer: retrieval falls back to the popularity
-- prior when the embedding table is empty (§7.1 R5), so a missing index is a cold shelf, not an
-- error.

-- ---------------------------------------------------------------------------------------
-- Collaborative signal
-- ---------------------------------------------------------------------------------------
-- Populated only for pairs meeting the minimum-support threshold, which is a privacy control
-- (§12.2) enforced in the aggregation that writes this table, not a filter applied on read.
CREATE TABLE series_cooccurrence (
  series_id uuid NOT NULL REFERENCES series(id) ON DELETE CASCADE,
  other_id  uuid NOT NULL REFERENCES series(id) ON DELETE CASCADE,
  support   int  NOT NULL,
  score     real NOT NULL,
  PRIMARY KEY (series_id, other_id),
  CHECK (series_id <> other_id),
  CHECK (support > 0)
);

-- ---------------------------------------------------------------------------------------
-- Appeal priors
-- ---------------------------------------------------------------------------------------
CREATE TABLE series_prior (
  series_id     uuid PRIMARY KEY REFERENCES series(id) ON DELETE CASCADE,
  prior         real    NOT NULL,
  watchers      int     NOT NULL DEFAULT 0,
  velocity      real    NOT NULL DEFAULT 0,
  recommendable boolean NOT NULL DEFAULT true,
  generation    int     NOT NULL,
  built_at      timestamptz NOT NULL DEFAULT now()
);

-- Cold start and shelf backfill read this ordering directly; partial, because an unrecommendable
-- series can never appear in it.
CREATE INDEX series_prior_top_idx ON series_prior (prior DESC) WHERE recommendable;

-- ---------------------------------------------------------------------------------------
-- Build bookkeeping
-- ---------------------------------------------------------------------------------------
-- Exactly one row, enforced by the primary key on a constant. A build is a singleton; a table
-- that can hold two states has two answers to "what generation is live?".
CREATE TABLE rec_build_state (
  id            boolean PRIMARY KEY DEFAULT true,
  generation    int  NOT NULL DEFAULT 0,
  stage         text NOT NULL DEFAULT 'idle',
  started_at    timestamptz,
  finished_at   timestamptz,
  series_built  int  NOT NULL DEFAULT 0,
  vocabulary    int  NOT NULL DEFAULT 0,
  dense_dims    int  NOT NULL DEFAULT 0,
  error         text,
  -- The projection's coefficients, little-endian `f32`, `basis_input_dim * dense_dims` of them.
  --
  -- Stored because an *incremental* build must project into the space the last full build
  -- defined. Re-solving from a partial catalogue would produce a different basis, and vectors
  -- from two bases are not comparable — the index would keep answering, with neighbours that are
  -- silently meaningless. This column is what makes the incremental path safe.
  basis           bytea,
  basis_input_dim int NOT NULL DEFAULT 0,
  CHECK (id)
);
INSERT INTO rec_build_state (id) VALUES (true);

-- Series whose model is stale for a reason the digest sweep would not otherwise notice.
--
-- The primary key is free deduplication, which is what makes this survive frequent merges: a
-- popular series enqueued forty times in an hour is one row.
CREATE TABLE rec_repair_queue (
  series_id   uuid PRIMARY KEY REFERENCES series(id) ON DELETE CASCADE,
  reason      text NOT NULL,
  enqueued_at timestamptz NOT NULL DEFAULT now()
);

ALTER TABLE rec_repair_queue
  ADD CONSTRAINT rec_repair_queue_reason_check
  CHECK (reason IN ('merged', 'features_changed'));
