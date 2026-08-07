-- Dropping `merge_decisions` discards every undo journal with it: after this runs, no automatic
-- merge taken before it can ever be reverted. That is not recoverable from anywhere else — the
-- absorbed rows only exist inside those journals — so a rollback past this point is a decision
-- to keep the merges.
-- Rows queued by a revert are dropped rather than kept under a reason the restored constraint
-- would reject: they are a request to recompute derived data, and the next build does that
-- anyway from a `features_changed` row or from scratch.
DELETE FROM rec_repair_queue WHERE reason = 'merge_reverted';
ALTER TABLE rec_repair_queue DROP CONSTRAINT rec_repair_queue_reason_check;
ALTER TABLE rec_repair_queue
  ADD CONSTRAINT rec_repair_queue_reason_check
  CHECK (reason IN ('merged', 'features_changed'));

DROP TABLE IF EXISTS sync_match_blocks;
DROP TABLE IF EXISTS sync_decisions;
DROP TABLE IF EXISTS merge_decisions;
