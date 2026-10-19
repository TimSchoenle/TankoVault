-- Retention for the merge journal's `deferred` rows.
--
-- The sweep journalled a `deferred` row every time its per-run merge ceiling put a pair off, so
-- the same pair was journalled on every run until the ceiling let it through: 1,800,193 rows in
-- production, most of them saying nothing the row before had not. The sweep now skips a deferral
-- whose pair's latest row is already `deferred`, and a maintenance pass deletes `deferred` rows
-- past a fixed retention. `merged`, `queued` and `distinct` rows are the undo journals and the
-- replay corpus and are kept indefinitely.
--
-- Partial, so the retention delete reads only the rows it may delete. Not `CONCURRENTLY`, for the
-- reason `0020_performance_indexes` gives; on a large database run it by hand first.
CREATE INDEX IF NOT EXISTS merge_decisions_deferred_idx ON merge_decisions (decided_at)
    WHERE outcome = 'deferred';
