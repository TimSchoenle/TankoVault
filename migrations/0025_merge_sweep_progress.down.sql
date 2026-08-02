-- The deleted alternative titles are not restored: they were provider-scraped label text, and
-- the rows that were legitimate are re-inserted by the next scan of each series.
DROP INDEX merge_candidates_distinct_recheck;

-- `distinct` rows would violate the narrowed constraint, so they are retired to the state the
-- previous schema used for a scorer verdict — no row at all.
DELETE FROM merge_candidates WHERE outcome = 'distinct';

ALTER TABLE merge_candidates DROP CONSTRAINT merge_candidates_outcome_check;
ALTER TABLE merge_candidates
  ADD CONSTRAINT merge_candidates_outcome_check
  CHECK (outcome IS NULL OR outcome IN ('merged', 'auto_merged', 'dismissed'));
