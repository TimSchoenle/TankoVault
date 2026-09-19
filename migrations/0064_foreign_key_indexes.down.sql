DROP INDEX IF EXISTS series_authors_author_idx;
DROP INDEX IF EXISTS series_tags_tag_idx;
DROP INDEX IF EXISTS user_provider_priority_provider_idx;
DROP INDEX IF EXISTS user_provider_early_access_provider_idx;

DROP INDEX IF EXISTS sync_conflicts_user_idx;
CREATE INDEX IF NOT EXISTS sync_conflicts_pending_idx ON sync_conflicts (user_id)
    WHERE resolved_at IS NULL;
DROP INDEX IF EXISTS user_recovery_codes_user_idx;
CREATE INDEX IF NOT EXISTS user_recovery_codes_unused_idx ON user_recovery_codes (user_id)
    WHERE used_at IS NULL;

DROP INDEX IF EXISTS user_permissions_granted_by_idx;
DROP INDEX IF EXISTS gdpr_requests_resolved_by_idx;
DROP INDEX IF EXISTS gdpr_requests_claimed_by_idx;
DROP INDEX IF EXISTS sync_match_blocks_created_by_idx;
DROP INDEX IF EXISTS sync_decisions_reverted_by_idx;
DROP INDEX IF EXISTS sync_decisions_flagged_by_idx;
DROP INDEX IF EXISTS series_merges_merged_by_idx;
DROP INDEX IF EXISTS merge_decisions_reverted_by_idx;
DROP INDEX IF EXISTS merge_decisions_flagged_by_idx;
DROP INDEX IF EXISTS merge_decisions_actor_idx;
DROP INDEX IF EXISTS merge_candidates_resolved_by_idx;
