-- The open-queue rotation had no index, and one index nothing reads survived its own replacement.
--
-- ---------------------------------------------------------------------------------------
-- The missing twin
-- ---------------------------------------------------------------------------------------
-- `0025_merge_sweep_progress` gave the *recheck* rotation the index its access pattern needs:
-- least-recently-scored first, partial on the outcome it drains, so the budget consumes an index
-- scan of exactly those rows. `open_merge_pairs` drains the open rows the same way —
-- `WHERE NOT resolved ORDER BY updated_at ASC LIMIT n` — and never got the matching index.
--
-- The two partial indexes that do exist are on `created_at DESC` and `(score DESC, created_at
-- DESC)`, so neither supplies the ordering: every sweep read the whole open set and top-N sorted
-- it to take 250 rows. That is invisible on a queue of hundreds and is not on a queue of
-- thousands, which is the size this queue is designed to survive.
CREATE INDEX merge_candidates_open_recheck ON merge_candidates (updated_at ASC)
  WHERE NOT resolved;

-- ---------------------------------------------------------------------------------------
-- Drop the index its own replacement made dead
-- ---------------------------------------------------------------------------------------
-- `merge_candidates_open` (0007) served the original queue read, `ORDER BY created_at DESC`.
-- `0023_merge_queue` replaced that ordering with `(score DESC, created_at DESC)` — the whole
-- point of that change being that insertion order is not a priority — and added
-- `merge_candidates_open_score` for it, but left this one in place. Nothing has ordered open rows
-- by `created_at` alone since; the only other reader of the partial predicate is the console's
-- `count(*) WHERE NOT resolved`, which any of the three serves as an index-only scan.
--
-- It is not free: every upsert in the sweep's hot path maintains it, and the sweep now writes the
-- queue in batches rather than row at a time, so the per-statement index maintenance is the cost
-- that is left.
DROP INDEX merge_candidates_open;
