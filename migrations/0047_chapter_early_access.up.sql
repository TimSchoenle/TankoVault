-- Early-access chapters: the provider-advertised paywall, and the per-reader opt-in that
-- decides whether one counts.
--
-- Several sources now onboarded sell early access — a chapter is published, listed, and
-- readable only to payers until a timer expires. Before this migration the ingest had exactly
-- two options for such a row, and both were wrong. Storing it as an ordinary chapter tells a
-- reader they are behind on something they cannot open, inflates every unread count, and fires
-- a "new chapter" notification for a page that answers with a paywall. Dropping it at ingest
-- loses the row that has to exist the moment the timer expires, and re-discovering it later
-- re-dates it — `discovered_at` would say "new today" for a chapter published a week ago,
-- which is the field the release feed orders by.
--
-- So the row is stored, and it is stored *as what it is*.

CREATE TYPE chapter_access AS ENUM ('free', 'early_access');

-- `access` is what the provider said at the last scan; `unlocks_at` is when it said the gate
-- opens. They are separate columns rather than one nullable timestamp because "locked, no date
-- given" and "free" are different answers and only one of them is safe to count: a provider
-- that shows a lock without a countdown (Toonily's coin chapters, HiveToons' permanent locks)
-- leaves `unlocks_at` NULL, and a NULL must never read as "already unlocked".
ALTER TABLE chapters
  ADD COLUMN access      chapter_access NOT NULL DEFAULT 'free',
  ADD COLUMN unlocks_at  timestamptz;

-- A free chapter cannot carry an unlock time. Enforced here and not only in the ingest, because
-- the ingest is not the only writer this table will ever have, and the read predicate below
-- treats a non-null `unlocks_at` as authoritative — a stray one on a free row would be
-- harmless, but a stray one left behind when a row *transitions* to free would not be.
ALTER TABLE chapters
  ADD CONSTRAINT chapters_unlocks_at_requires_early_access
  CHECK (access = 'early_access' OR unlocks_at IS NULL);

-- ---------------------------------------------------------------------------------------
-- Keeping the unread predicate index-only
-- ---------------------------------------------------------------------------------------
-- `0026_unread_covering_index` made the unread predicate answerable from the index alone, and
-- documents at length what it cost when it was not (2.9 s on the Home stats query). Every one
-- of those five copies of the predicate now also tests `access`/`unlocks_at`, so both columns
-- have to be reachable from the same index or the fix in 0026 is undone — one heap fetch per
-- chapter of every watched series, exactly as before.
--
-- They ride along as INCLUDE payload rather than key columns: nothing orders or ranges by
-- them, they are only ever read as a filter on a row the key columns already found.
--
-- Not `CREATE INDEX CONCURRENTLY`, for the reason `0020_performance_indexes` sets out — sqlx
-- sends the file as one simple-query string, which Postgres wraps in an implicit transaction.
-- On a database whose `chapters` table is already large, run this by hand first; the
-- statements are `IF NOT EXISTS`/`IF EXISTS`, so the migration then finds the work done:
--
--   CREATE INDEX CONCURRENTLY IF NOT EXISTS chapters_source_floor_num_access_idx
--       ON chapters (series_source_id, (floor(number)), number) INCLUDE (access, unlocks_at);
--   DROP INDEX CONCURRENTLY IF EXISTS chapters_source_floor_num_idx;
CREATE INDEX IF NOT EXISTS chapters_source_floor_num_access_idx
    ON chapters (series_source_id, (floor(number)), number) INCLUDE (access, unlocks_at);

DROP INDEX IF EXISTS chapters_source_floor_num_idx;

-- ---------------------------------------------------------------------------------------
-- The per-reader, per-provider opt-in
-- ---------------------------------------------------------------------------------------
-- Presence of a row means "count this provider's early-access chapters for me". There is no
-- third state to record — the default is off and a reader either opts in or does not — so a
-- boolean column would only add a way for the table to disagree with itself.
--
-- It is per *provider* and not global on purpose. A reader who subscribes to one scanlator's
-- early access has bought exactly that site's chapters; the same setting applied to a site they
-- do not pay for would put chapters they cannot open back into their unread count, which is the
-- problem this whole migration exists to fix.
CREATE TABLE user_provider_early_access (
  user_id     uuid NOT NULL REFERENCES users(id)     ON DELETE CASCADE,
  provider_id uuid NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
  enabled_at  timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (user_id, provider_id)
);

-- The read paths resolve the reader's opt-in set once per request and pass it down as a
-- `uuid[]`, so the lookup is this one index scan and not an `EXISTS` re-run per chapter row.
CREATE INDEX user_provider_early_access_user_idx
    ON user_provider_early_access (user_id);
