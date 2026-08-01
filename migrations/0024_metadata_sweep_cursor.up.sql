-- Enrichment bookkeeping: when the metadata sweep last *attempted* this series.
--
-- The sweep used to page the catalogue by `series.updated_at` — the same column
-- `apply_enrichment` writes. Paging by a column the pass mutates was already fragile (migration
-- 0020 replaced an `OFFSET` walk with a keyset one for exactly that reason), but the deeper
-- problem is that `updated_at` records a *change*, and a sweep needs to know about an *attempt*:
-- a series no provider could resolve was written nowhere, kept its old `updated_at`, stayed at
-- the head of the ascending order, and was retried at the front of every following sweep. With
-- more unresolvable series than the per-run cap, the sweep never reached anything else — it
-- re-tried the same failures hourly and reported a full walk.
--
-- `metadata_checked_at` is stamped on every attempt, resolved or not, so each series takes its
-- turn before any is revisited.
ALTER TABLE series ADD COLUMN IF NOT EXISTS metadata_checked_at timestamptz;

-- Never-checked series first, then least-recently-checked. `NULLS FIRST` is spelled out because
-- an ascending btree defaults to `NULLS LAST`, which would order the never-checked rows — the
-- ones the sweep exists to reach — dead last.
CREATE INDEX IF NOT EXISTS series_metadata_sweep_idx
    ON series (metadata_checked_at ASC NULLS FIRST, id ASC);

-- The ascending `(updated_at, id)` half of migration 0020, added solely for the old enrichment
-- cursor. Nothing orders `series` ascending by `updated_at` any more; `series_updated_idx` still
-- covers the descending catalogue listings.
DROP INDEX IF EXISTS series_enrichment_cursor_idx;
