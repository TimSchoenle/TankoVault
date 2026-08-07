-- The adult-content gate: a second classifier for `series`, and the per-reader opt-in that is
-- the only thing which can open it.
--
-- Until now `series.is_adult` was written by exactly one source — AniList's `isAdult`, through
-- the enrichment sweep — and it gated three recommender retrieval paths and nothing else. Two
-- holes: a series the sweep has never matched keeps the column's `false` default and reads as
-- safe, and browse, search and series detail never consulted the flag at all.

-- ---------------------------------------------------------------------------------------
-- A second, independent classifier
-- ---------------------------------------------------------------------------------------
-- `adult_inferred` is the ingest-time verdict from the provider's own genre chips, kept in its
-- own column rather than folded into `is_adult` because the two have different writers with
-- different authority. AniList is authoritative and may say either yes or no; the ingest
-- classifier is a heuristic over scraped strings and may only ever say yes. Sharing one column
-- would mean an AniList refresh answering `isAdult: false` silently clears a verdict it never
-- considered — a gate that opens itself during a routine metadata sweep.
ALTER TABLE series
  ADD COLUMN IF NOT EXISTS adult_inferred boolean NOT NULL DEFAULT false;

-- The gate every read surface actually tests. Generated, not a predicate the callers spell out:
-- this repository has already paid for the same condition copied into five queries and drifting,
-- and a gate is the worst possible place to discover the sixth copy was written wrong. Adding a
-- third classifier later changes this line and nothing else.
ALTER TABLE series
  ADD COLUMN IF NOT EXISTS adult_gated boolean
  GENERATED ALWAYS AS (is_adult OR adult_inferred) STORED;

-- Partial, on the *excluded* side. The overwhelming majority of rows are not gated, so an index
-- over the whole column earns nothing on the common `NOT adult_gated` browse path — the planner
-- filters those rows either way. What this index is for is the admin surfaces that ask the
-- opposite question ("what is currently gated?"), where the answer is a small fraction of a
-- large table and a sequential scan is the wrong plan.
CREATE INDEX IF NOT EXISTS series_adult_gated_idx ON series (id) WHERE adult_gated;

-- ---------------------------------------------------------------------------------------
-- The per-reader opt-in
-- ---------------------------------------------------------------------------------------
-- Two columns, not one, because they answer different questions and are revoked separately: an
-- account may attest once and then toggle the preference off and on again without re-attesting,
-- but it can never hold the preference without having attested at all.
--
-- Both live on `users` rather than in the `notification_prefs` JSON blob next door. A gate is
-- queried in predicates by the read paths, and a JSON extraction in a `WHERE` clause is neither
-- indexable nor type-checked at compile time by `sqlx`. It is also the difference between a
-- schema that records consent and one that stores it as an untyped bag.
ALTER TABLE users
  ADD COLUMN IF NOT EXISTS adult_opt_in    boolean NOT NULL DEFAULT false,
  ADD COLUMN IF NOT EXISTS age_attested_at timestamptz;

-- The invariant, enforced in the schema and not only in the handler that writes it. The API is
-- not the only writer this table will ever have — a future admin tool, a data import, or a
-- support fix applied by hand all bypass whatever the handler checks. An opt-in with no
-- attestation behind it is exactly the state this whole migration exists to make unreachable.
ALTER TABLE users DROP CONSTRAINT IF EXISTS users_adult_opt_in_requires_attestation;
ALTER TABLE users
  ADD CONSTRAINT users_adult_opt_in_requires_attestation
  CHECK (NOT adult_opt_in OR age_attested_at IS NOT NULL);
