DROP INDEX IF EXISTS merge_candidates_open_recheck;

-- Restored exactly as `0007_moderation.sql` declared it.
CREATE INDEX merge_candidates_open ON merge_candidates (created_at DESC) WHERE NOT resolved;
