-- Index the remaining foreign keys whose parent is deleted in normal operation.
--
-- 0063 covered the catalogue parents. Account erasure (`DELETE FROM users`) and provider deletion
-- run the same per-row `ON DELETE` action against every referencing table, and each of the
-- columns below made that a sequential scan of a table that grows with users or the catalogue.
-- Pinned by `crates/db/tests/schema_foreign_key_indexes.rs`, which lists the few foreign keys
-- deliberately left unindexed and why.
--
-- Not `CREATE INDEX CONCURRENTLY`, for the reason `0020_performance_indexes` gives. On a large
-- database, run each CREATE below by hand with `CONCURRENTLY` first; the migration then finds
-- the work done.

-- Actor columns (`ON DELETE SET NULL`). Mostly NULL, so partial; an equality on the column
-- implies `IS NOT NULL`, which lets the referential action use them.
CREATE INDEX IF NOT EXISTS merge_candidates_resolved_by_idx ON merge_candidates (resolved_by)
    WHERE resolved_by IS NOT NULL;
CREATE INDEX IF NOT EXISTS merge_decisions_actor_idx ON merge_decisions (actor_id)
    WHERE actor_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS merge_decisions_flagged_by_idx ON merge_decisions (flagged_by)
    WHERE flagged_by IS NOT NULL;
CREATE INDEX IF NOT EXISTS merge_decisions_reverted_by_idx ON merge_decisions (reverted_by)
    WHERE reverted_by IS NOT NULL;
CREATE INDEX IF NOT EXISTS series_merges_merged_by_idx ON series_merges (merged_by)
    WHERE merged_by IS NOT NULL;
CREATE INDEX IF NOT EXISTS sync_decisions_flagged_by_idx ON sync_decisions (flagged_by)
    WHERE flagged_by IS NOT NULL;
CREATE INDEX IF NOT EXISTS sync_decisions_reverted_by_idx ON sync_decisions (reverted_by)
    WHERE reverted_by IS NOT NULL;
CREATE INDEX IF NOT EXISTS sync_match_blocks_created_by_idx ON sync_match_blocks (created_by)
    WHERE created_by IS NOT NULL;
CREATE INDEX IF NOT EXISTS gdpr_requests_claimed_by_idx ON gdpr_requests (claimed_by)
    WHERE claimed_by IS NOT NULL;
CREATE INDEX IF NOT EXISTS gdpr_requests_resolved_by_idx ON gdpr_requests (resolved_by)
    WHERE resolved_by IS NOT NULL;
CREATE INDEX IF NOT EXISTS user_permissions_granted_by_idx ON user_permissions (granted_by)
    WHERE granted_by IS NOT NULL;

-- Owner columns whose only index was partial on a state predicate the cascade does not carry.
-- The total index replaces it: a user holds at most ten recovery codes, and pending conflicts
-- are still served by `sync_conflicts_unique_pending_idx`, which leads on `user_id`.
DROP INDEX IF EXISTS user_recovery_codes_unused_idx;
CREATE INDEX IF NOT EXISTS user_recovery_codes_user_idx ON user_recovery_codes (user_id);
DROP INDEX IF EXISTS sync_conflicts_pending_idx;
CREATE INDEX IF NOT EXISTS sync_conflicts_user_idx ON sync_conflicts (user_id);

-- Provider deletion.
CREATE INDEX IF NOT EXISTS user_provider_early_access_provider_idx
    ON user_provider_early_access (provider_id);
CREATE INDEX IF NOT EXISTS user_provider_priority_provider_idx
    ON user_provider_priority (provider_id);

-- Vocabulary deletion (0053's repair deleted tags and authors in bulk). The second column makes
-- the per-tag and per-author reads index-only.
CREATE INDEX IF NOT EXISTS series_tags_tag_idx ON series_tags (tag_id, series_id);
CREATE INDEX IF NOT EXISTS series_authors_author_idx ON series_authors (author_id, series_id);
