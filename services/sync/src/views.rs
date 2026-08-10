//! Repository rows → the published `/v1/sync/*` bodies.
//!
//! Free functions rather than `From` impls: both sides are foreign to this crate, so the orphan
//! rule forbids the impl, and neither `tankovault-db` nor `tankovault-contracts` may depend on
//! the other — they are siblings over `tankovault-domain`, and db's dependency list is
//! deliberately narrow (ARCH-16).
//!
//! Why the hop exists at all is the same reason `crates/contracts/src/admin.rs` gives for the
//! console: a `SELECT` column rename must be a compile error, not a silent rewrite of the public
//! API. `services/api` re-publishes these bodies verbatim, so before this module the two ends
//! agreed only by field names happening to match, and a renamed column surfaced as a `502` from
//! the API's `Upstream::decode` at runtime.

use tankovault_contracts::admin::SyncDecisionView;
use tankovault_contracts::sync::{ConflictView, HistoryView};
use tankovault_db::repo::sync::{ConflictRow, HistoryRow, SyncDecisionRow};

/// One pending conflict, as `GET /v1/sync/conflicts/{user_id}` publishes it.
#[must_use]
pub(crate) fn conflict_view(row: ConflictRow) -> ConflictView {
    ConflictView {
        id: row.id,
        series_id: row.series_id,
        series_title: row.series_title,
        provider: row.provider,
        field: row.field,
        local_value: row.local_value,
        remote_value: row.remote_value,
        detected_at: row.detected_at,
    }
}

/// One history entry, as `GET /v1/sync/history/{user_id}` publishes it.
#[must_use]
pub(crate) fn history_view(row: HistoryRow) -> HistoryView {
    HistoryView {
        id: row.id,
        series_id: row.series_id,
        series_title: row.series_title,
        provider: row.provider,
        action: row.action,
        detail: row.detail,
        created_at: row.created_at,
    }
}

/// One journal row, as `GET /v1/sync/decisions` publishes it.
#[must_use]
pub(crate) fn decision_view(row: SyncDecisionRow) -> SyncDecisionView {
    SyncDecisionView {
        id: row.id,
        run_id: row.run_id,
        decided_at: row.decided_at,
        user_id: row.user_id,
        username: row.username,
        series_id: row.series_id,
        series_title: row.series_title,
        provider: row.provider,
        external_id: row.external_id,
        scope: row.scope,
        action: row.action,
        reason: row.reason,
        policy: row.policy,
        applied: row.applied,
        local_before: row.local_before,
        local_after: row.local_after,
        remote_before: row.remote_before,
        remote_after: row.remote_after,
        ancestor_local: row.ancestor_local,
        ancestor_remote: row.ancestor_remote,
        match_score: row.match_score,
        match_signals: row.match_signals,
        evidence: row.evidence,
        reverted_at: row.reverted_at,
        reverted_by: row.reverted_by,
        revert_reason: row.revert_reason,
        flagged_at: row.flagged_at,
        flagged_by: row.flagged_by,
        flag_reason: row.flag_reason,
    }
}
