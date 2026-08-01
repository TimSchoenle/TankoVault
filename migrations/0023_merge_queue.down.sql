-- Reverse of `0023_merge_queue.up.sql`, as far as reversing it means anything.
--
-- The schema comes back exactly. The *data* does not, and cannot: the up-migration re-derived
-- every `normalized_title` and `series_titles.normalized` under the corrected rules, dropping
-- alternative-title rows that the correction collapsed onto one key, and collapsed duplicate
-- merge-candidate rows. Recomputing the old keys would mean re-implementing a normalizer that
-- no longer exists in the tree, and the dropped rows are gone regardless — so this migration
-- deliberately leaves the keys corrected. Rolling back the code without rolling back the keys
-- is the safe direction: the old matcher reads the new keys as ordinary titles and merely
-- matches less well, whereas half-reverted keys would match *wrongly*.
DROP INDEX IF EXISTS merge_candidates_open_score;

ALTER TABLE merge_candidates DROP CONSTRAINT IF EXISTS merge_candidates_outcome_check;
ALTER TABLE merge_candidates DROP COLUMN IF EXISTS outcome;
ALTER TABLE merge_candidates DROP COLUMN IF EXISTS updated_at;
ALTER TABLE merge_candidates DROP COLUMN IF EXISTS signals;

DROP INDEX IF EXISTS merge_candidates_pair_key;

DROP INDEX IF EXISTS series_titles_compact_idx;
DROP INDEX IF EXISTS series_compact_title_idx;

DROP FUNCTION IF EXISTS tv_normalize_title(text);
