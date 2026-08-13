-- The pruned tags, credits and feature rows are not restored: they were scrape-template label
-- text, and any of them a deployment's own guard allows is re-inserted by the next scan of each
-- series, because both writers are idempotent upserts. The vectors repaired in place are derived
-- and converge on the next build either way.

-- Rows on the retired reason would violate the narrowed constraint.
DELETE FROM rec_repair_queue WHERE reason = 'vocabulary_pruned';

ALTER TABLE rec_repair_queue DROP CONSTRAINT rec_repair_queue_reason_check;
ALTER TABLE rec_repair_queue
  ADD CONSTRAINT rec_repair_queue_reason_check
  CHECK (reason IN ('merged', 'features_changed', 'merge_reverted'));
