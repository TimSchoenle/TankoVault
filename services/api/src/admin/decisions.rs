//! The decision journals: why the automatic merge and the automatic sync did what they did, and
//! the two operator actions that answer "it was wrong" — undo it, and say so durably.
//!
//! Both surfaces sit behind their own permissions rather than the existing `merge.write` /
//! `sync.admin.write`. Reading a journal discloses more than working the queue it belongs to (the
//! sync one carries every reader's progress history), and undoing an automatic merge is the only
//! action in the system that brings a deleted series back.

use crate::audit::audit;
use crate::error::{ApiError, ApiResult};
use crate::openapi::{ADMIN_MATCHING_TAG, ADMIN_SYNC_TAG};
use crate::state::{AppState, AuthUser};
use crate::views::IntoView;
use axum::Json;
use axum::extract::{Path, Query, State};
use serde::Deserialize;
use tankovault_contracts::admin::{
    MergeDecisionView, MergeRevertedView, SyncDecisionView, SyncRevertedView,
};
use tankovault_domain::{Permission, SeriesId};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

/// Page size cap. Generous because the journal is read to *find* a decision, and an operator who
/// has to page through a sweep run's output twenty rows at a time will not.
const MAX_DECISIONS: i64 = 200;

/// Longest reason an operator may attach to a revert or a flag.
///
/// Bounded because it is stored and rendered; the cap is far above a sentence and far below
/// anything that would make the journal expensive to read.
const MAX_REASON: usize = 1000;

/// Reject an empty or oversized reason before it reaches the database.
///
/// A revert and a flag are both durable judgements that suppress a pair forever, and an unlabelled
/// one is unattributable six months later — which is when it is read.
fn check_reason(reason: &str) -> ApiResult<()> {
    let trimmed = reason.trim();
    if trimmed.is_empty() {
        return Err(ApiError::BadRequest(
            "a reason is required: this judgement is durable and will be read without you"
                .to_owned(),
        ));
    }
    if trimmed.chars().count() > MAX_REASON {
        return Err(ApiError::BadRequest(format!(
            "reason is longer than {MAX_REASON} characters"
        )));
    }
    Ok(())
}

#[derive(Debug, Default, Deserialize, IntoParams)]
pub struct MergeDecisionFilter {
    /// Restrict to one outcome: `merged`, `queued`, `requeued`, `reopened`, `withdrawn`,
    /// `distinct`, `deferred`.
    #[serde(default)]
    pub outcome: Option<String>,
    /// Only decisions naming this series on either side. Survives the merge — an absorbed id is
    /// still on the row that absorbed it, which is the one you go looking for.
    ///
    /// A bare `Uuid`, not `SeriesId`: an optional newtype in a query parameter generates a
    /// one-variant `oneOf` that the client generator cannot render.
    #[serde(default)]
    pub series_id: Option<Uuid>,
    /// Only merges that can still be undone.
    #[serde(default)]
    pub revertible: bool,
    /// Only decisions an operator has flagged wrong.
    #[serde(default)]
    pub flagged: bool,
    /// Only decisions a guard held back: the near misses.
    #[serde(default)]
    pub blocked: bool,
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub offset: Option<i64>,
}

/// List merge decisions
///
/// The automatic-merge journal, newest first: the itemised score behind each decision, the rule
/// that produced the verdict, the guards that overrode it, and whether it can still be undone.
#[utoipa::path(
    get,
    path = "/v1/admin/merge-decisions",
    tag = ADMIN_MATCHING_TAG,
    params(MergeDecisionFilter),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "A page of the merge decision journal", body = Vec<MergeDecisionView>),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
    )
)]
pub async fn list_merge_decisions(
    State(state): State<AppState>,
    user: AuthUser,
    Query(filter): Query<MergeDecisionFilter>,
) -> ApiResult<Json<Vec<MergeDecisionView>>> {
    user.require(Permission::MergeAudit).await?;
    let rows = tankovault_db::repo::matching::list_merge_decisions(
        &state.pool,
        &tankovault_db::repo::matching::MergeDecisionFilter {
            outcome: filter.outcome,
            series_id: filter.series_id.map(SeriesId::from_uuid),
            revertible_only: filter.revertible,
            flagged_only: filter.flagged,
            blocked_only: filter.blocked,
        },
        filter.limit.unwrap_or(50).clamp(1, MAX_DECISIONS),
        filter.offset.unwrap_or(0).max(0),
    )
    .await?;
    Ok(Json(rows.into_view()))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct JudgementRequest {
    /// Why. Required, stored, and rendered beside the decision forever.
    pub reason: String,
}

/// Revert a merge
///
/// Undo one automatic merge: the absorbed series exists again under its **original id**, every
/// row that moved is moved back, and every row the merge created on the survivor is removed.
///
/// The pair is suppressed as part of the same action. Reverting alone changes nothing about why
/// the two were merged — the titles still agree and the score is still above the threshold — so
/// the next sweep would simply merge them again.
#[utoipa::path(
    post,
    path = "/v1/admin/merge-decisions/{id}/revert",
    tag = ADMIN_MATCHING_TAG,
    params(("id" = Uuid, Path, description = "The merge decision to undo")),
    request_body = JudgementRequest,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "What was put back", body = MergeRevertedView),
        (status = 400, description = "no reason given", body = crate::error::ProblemDetails),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
        (status = 404, description = "no such decision", body = crate::error::ProblemDetails),
        (status = 409, description = "already reverted, merged nothing, or the absorbed id is live again", body = crate::error::ProblemDetails),
    )
)]
pub async fn revert_merge_decision(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(req): Json<JudgementRequest>,
) -> ApiResult<Json<MergeRevertedView>> {
    user.require(Permission::MergeRevert).await?;
    check_reason(&req.reason)?;
    let undo = tankovault_db::repo::matching::revert_merge_decision(
        &state.pool,
        id,
        Some(user.user_id),
        req.reason.trim(),
    )
    .await?;

    let view = MergeRevertedView {
        decision_id: id,
        restored_id: SeriesId::from_uuid(undo.absorbed_id),
        survivor_id: SeriesId::from_uuid(undo.survivor_id),
        rows_restored: i64::try_from(undo.row_count()).unwrap_or(i64::MAX),
        pair_suppressed: true,
    };
    audit(
        &state,
        &user,
        "merge_decision.revert",
        &id.to_string(),
        &serde_json::json!({
            "restored": view.restored_id,
            "survivor": view.survivor_id,
            "rows": view.rows_restored,
            "reason": req.reason.trim(),
        }),
    )
    .await;
    Ok(Json(view))
}

/// Flag a merge as wrong
///
/// Record that an automatic merge was the wrong call, and suppress the pair permanently, without
/// undoing it. Deliberately independent of the revert: a merge can be wrong and no longer worth
/// the disruption of unpicking, and a merge can be undone as a precaution while still having been
/// correct. Either way the flag is what stops the sweep re-making the same decision.
#[utoipa::path(
    post,
    path = "/v1/admin/merge-decisions/{id}/flag",
    tag = ADMIN_MATCHING_TAG,
    params(("id" = Uuid, Path, description = "The merge decision to flag")),
    request_body = JudgementRequest,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Whether this call was the one that flagged it", body = serde_json::Value, example = json!({"flagged": true})),
        (status = 400, description = "no reason given", body = crate::error::ProblemDetails),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
        (status = 404, description = "no such decision", body = crate::error::ProblemDetails),
    )
)]
pub async fn flag_merge_decision(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(req): Json<JudgementRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    user.require(Permission::MergeRevert).await?;
    check_reason(&req.reason)?;
    let flagged = tankovault_db::repo::matching::flag_merge_decision(
        &state.pool,
        id,
        Some(user.user_id),
        req.reason.trim(),
    )
    .await?;
    audit(
        &state,
        &user,
        "merge_decision.flag",
        &id.to_string(),
        &serde_json::json!({ "flagged": flagged, "reason": req.reason.trim() }),
    )
    .await;
    Ok(Json(serde_json::json!({ "flagged": flagged })))
}

#[derive(Debug, Default, Deserialize, IntoParams)]
pub struct SyncDecisionFilter {
    #[serde(default)]
    pub user_id: Option<Uuid>,
    /// A bare `Uuid` for the same reason as above.
    #[serde(default)]
    pub series_id: Option<Uuid>,
    #[serde(default)]
    pub provider: Option<String>,
    /// Restrict to one action: `matched`, `unmatched`, `pull`, `push`, `conflict`, `noop`, …
    #[serde(default)]
    pub action: Option<String>,
    /// One reconciliation run.
    #[serde(default)]
    pub run_id: Option<Uuid>,
    /// Only decisions that wrote something. A run is mostly considerations, so this is the
    /// filter to reach for when asking "what did the sync change?".
    #[serde(default)]
    pub applied: bool,
    #[serde(default)]
    pub flagged: bool,
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub offset: Option<i64>,
}

/// List sync decisions
///
/// The automatic-sync journal across every user, newest first. Covers what the per-user sync
/// history never did: the remote entries that matched nothing, the series skipped as excluded,
/// the fields both sides already agreed on, and the scored title match behind every mapping.
#[utoipa::path(
    get,
    path = "/v1/admin/sync/decisions",
    tag = ADMIN_SYNC_TAG,
    params(SyncDecisionFilter),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "A page of the sync decision journal", body = Vec<SyncDecisionView>),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
    )
)]
pub async fn list_sync_decisions(
    State(state): State<AppState>,
    user: AuthUser,
    Query(filter): Query<SyncDecisionFilter>,
) -> ApiResult<Json<Vec<SyncDecisionView>>> {
    user.require(Permission::SyncAudit).await?;
    let rows = tankovault_db::repo::sync::list_sync_decisions(
        &state.pool,
        &tankovault_db::repo::sync::SyncDecisionFilter {
            user_id: filter.user_id.map(tankovault_domain::UserId::from_uuid),
            series_id: filter.series_id.map(SeriesId::from_uuid),
            provider: filter.provider,
            action: filter.action,
            run_id: filter.run_id,
            applied_only: filter.applied,
            flagged_only: filter.flagged,
        },
        filter.limit.unwrap_or(50).clamp(1, MAX_DECISIONS),
        filter.offset.unwrap_or(0).max(0),
    )
    .await?;
    Ok(Json(rows.into_view()))
}

/// Revert a sync decision
///
/// Undo one journalled sync action. Forwarded to the sync service, which is the tier holding the
/// provider token: taking back a *push* means writing the remote's previous values back, and only
/// that service can.
///
/// Not every action has an inverse. A series the sync created on the remote cannot be removed
/// from here — no provider in this system exposes a delete — and that case answers with an error
/// saying so rather than silently doing half of it.
#[utoipa::path(
    post,
    path = "/v1/admin/sync/decisions/{id}/revert",
    tag = ADMIN_SYNC_TAG,
    params(("id" = Uuid, Path, description = "The sync decision to undo")),
    request_body = JudgementRequest,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "What was put back", body = SyncRevertedView),
        (status = 400, description = "no reason given", body = crate::error::ProblemDetails),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
        (status = 404, description = "no such decision", body = crate::error::ProblemDetails),
        (status = 409, description = "already reverted, changed nothing, or has no inverse", body = crate::error::ProblemDetails),
    )
)]
pub async fn revert_sync_decision(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(req): Json<JudgementRequest>,
) -> ApiResult<Json<SyncRevertedView>> {
    user.require(Permission::SyncRevert).await?;
    check_reason(&req.reason)?;
    let Json(view): Json<SyncRevertedView> = state
        .sync
        .post(
            &format!("/v1/sync/decisions/{id}/revert"),
            &serde_json::json!({ "actor": user.user_id, "reason": req.reason.trim() }),
        )
        .await?;
    audit(
        &state,
        &user,
        "sync_decision.revert",
        &id.to_string(),
        &serde_json::json!({
            "restored": view.restored, "value": view.value, "reason": req.reason.trim(),
        }),
    )
    .await;
    Ok(Json(view))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SyncFlagRequest {
    /// Why. Required, stored, and rendered beside the decision forever.
    pub reason: String,
    /// Also refuse the (external id, series) match this decision made, permanently.
    ///
    /// Deleting the mapping alone fixes nothing: the next reconciliation runs the same title
    /// match against the same catalogue and writes the same row back.
    #[serde(default)]
    pub block_match: bool,
}

/// Flag a sync decision as wrong
///
/// Record that an automatic sync action was the wrong call without undoing it, and — with
/// `block_match` — refuse the title match it made so no later run can re-make it.
#[utoipa::path(
    post,
    path = "/v1/admin/sync/decisions/{id}/flag",
    tag = ADMIN_SYNC_TAG,
    params(("id" = Uuid, Path, description = "The sync decision to flag")),
    request_body = SyncFlagRequest,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Whether this call was the one that flagged it", body = serde_json::Value, example = json!({"flagged": true})),
        (status = 400, description = "no reason given", body = crate::error::ProblemDetails),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
        (status = 404, description = "no such decision", body = crate::error::ProblemDetails),
    )
)]
pub async fn flag_sync_decision(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(req): Json<SyncFlagRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    user.require(Permission::SyncRevert).await?;
    check_reason(&req.reason)?;
    let Json(body): Json<serde_json::Value> = state
        .sync
        .post(
            &format!("/v1/sync/decisions/{id}/flag"),
            &serde_json::json!({
                "actor": user.user_id,
                "reason": req.reason.trim(),
                "block_match": req.block_match,
            }),
        )
        .await?;
    audit(
        &state,
        &user,
        "sync_decision.flag",
        &id.to_string(),
        &serde_json::json!({ "block_match": req.block_match, "reason": req.reason.trim() }),
    )
    .await;
    Ok(Json(body))
}
