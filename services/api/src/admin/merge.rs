//! The duplicate-series merge queue: candidates, merges, dismissals, the standing sweep, and
//! the normalized-key rebuild the sweep depends on.

use crate::audit::audit;
use crate::error::ApiResult;
use crate::openapi::ADMIN_MATCHING_TAG;
use crate::state::{AppState, AuthUser};
use crate::views::IntoView;
use axum::Json;
use axum::extract::{Query, State};
use serde::Deserialize;
use tankovault_contracts::admin::{KeyRebuildView, MergeCandidateView, MergeSweepView};
use tankovault_domain::{Permission, SeriesId};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

/// Page size cap for the review queue; safe because results are score-ordered, so a cap only
/// trims the low-confidence end.
const MAX_CANDIDATES: i64 = 200;

#[derive(Debug, Default, Deserialize, IntoParams)]
pub struct CandidateFilter {
    /// Only return candidates at or above this confidence, in `[0,1]`.
    ///
    /// An operator works a large queue in bands: everything above 0.9 is nearly all genuine
    /// duplicates and can be actioned quickly, while the 0.6–0.7 band needs real attention per
    /// row. Out-of-range values are clamped rather than rejected — this narrows a list, so the
    /// worst a nonsense value can do is return nothing.
    #[serde(default)]
    pub min_score: Option<f32>,
}

/// List merge candidates
///
/// The canonicalisation review queue (design §10), highest confidence first.
#[utoipa::path(
    get,
    path = "/v1/admin/merge-candidates",
    tag = ADMIN_MATCHING_TAG,
    params(CandidateFilter),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Up to 200 open merge candidates, highest score first", body = Vec<MergeCandidateView>),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
    )
)]
pub async fn list_merge_candidates(
    State(state): State<AppState>,
    user: AuthUser,
    Query(filter): Query<CandidateFilter>,
) -> ApiResult<Json<Vec<MergeCandidateView>>> {
    user.require(Permission::MergeRead).await?;
    let min_score = filter.min_score.unwrap_or(0.0).clamp(0.0, 1.0);
    let rows = tankovault_db::repo::matching::list_open_merge_candidates(
        &state.pool,
        MAX_CANDIDATES,
        min_score,
    )
    .await?;
    Ok(Json(rows.into_view()))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct MergeRequest {
    /// The surviving canonical series.
    pub keep: SeriesId,
    /// The series merged into `keep` and then deleted.
    pub merge: SeriesId,
}

/// Merge two series
///
/// Transactional re-parent + title/tag/progress/sync union (design §10).
#[utoipa::path(
    post,
    path = "/v1/admin/series/merge",
    tag = ADMIN_MATCHING_TAG,
    request_body = MergeRequest,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Acknowledged", body = serde_json::Value, example = json!({"ok": true})),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
        (status = 404, description = "One or both series not found", body = crate::error::ProblemDetails),
        (status = 409, description = "`keep` and `merge` are the same series", body = crate::error::ProblemDetails),
    )
)]
pub async fn merge_series(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<MergeRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    user.require(Permission::MergeWrite).await?;
    tankovault_db::repo::matching::merge_series(
        &state.pool,
        req.keep,
        req.merge,
        Some(user.user_id),
        "merged",
    )
    .await?;
    audit(
        &state,
        &user,
        "series.merge",
        &req.merge.to_string(),
        &serde_json::json!({ "keep": req.keep, "merged": req.merge }),
    )
    .await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct DismissRequest {
    pub id: Uuid,
}

/// Dismiss a merge candidate
///
/// Operator judged the two works distinct. The judgement is durable: the pair is suppressed
/// against re-scans and against the standing duplicate sweep, which is what the previous
/// implementation could not promise — a dismissed pair could be re-inserted as a fresh open row
/// by the next scan that saw it.
#[utoipa::path(
    post,
    path = "/v1/admin/merge-candidates/dismiss",
    tag = ADMIN_MATCHING_TAG,
    request_body = DismissRequest,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Whether a candidate was actually dismissed", body = serde_json::Value, example = json!({"dismissed": true})),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
    )
)]
pub async fn dismiss_merge_candidate(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<DismissRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    user.require(Permission::MergeWrite).await?;
    let dismissed = tankovault_db::repo::matching::dismiss_merge_candidate(
        &state.pool,
        req.id,
        Some(user.user_id),
    )
    .await?;
    audit(
        &state,
        &user,
        "merge_candidate.dismiss",
        &req.id.to_string(),
        &serde_json::json!({ "dismissed": dismissed }),
    )
    .await;
    Ok(Json(serde_json::json!({ "dismissed": dismissed })))
}

/// Run the duplicate sweep
///
/// Re-blocks the whole catalogue on the whitespace-insensitive title key, re-scores every
/// shortlisted pair with everything now known about both series, merges the certain ones and
/// queues the rest.
///
/// Forwarded to the control-plane, which owns the schedule and the leader election that keeps
/// two replicas from racing on the same destructive merge. The scheduled sweep is the normal
/// path; this is here so an operator changing a threshold can see the effect on one run.
#[utoipa::path(
    post,
    path = "/v1/admin/merge-candidates/sweep",
    tag = ADMIN_MATCHING_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "What the sweep did", body = MergeSweepView),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
        (status = 404, description = "automatic duplicate merging is switched off", body = crate::error::ProblemDetails),
    )
)]
pub async fn sweep_merge_candidates(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<MergeSweepView>> {
    user.require(Permission::MergeWrite).await?;
    // The actor travels with the request so every candidate the sweep resolves records *who*
    // asked for it, rather than being indistinguishable from the hourly schedule.
    let Json(report): Json<MergeSweepView> = state
        .control_plane
        .post(
            "/internal/merge-sweep",
            &serde_json::json!({ "actor": user.user_id }),
        )
        .await?;
    audit(
        &state,
        &user,
        "merge.sweep",
        "-",
        &serde_json::to_value(report).unwrap_or_default(),
    )
    .await;
    Ok(Json(report))
}

/// Rebuild the normalized matching keys
///
/// Re-derives `series.normalized_title` and `series_titles.normalized` for the whole catalogue
/// through the current `normalize_title`.
///
/// # Why this is an operator action
///
/// The normalized title is a *persisted* key: it is written once, when a series is created, and
/// every later match compares against the stored value. A change to the normalization rules —
/// like making an apostrophe join a word instead of splitting one — therefore leaves the entire
/// catalogue on keys derived by the previous rules, and the improvement only reaches rows that
/// happen to be re-scanned. Running this is what makes a rules change take effect, and it is
/// safe to run repeatedly: only rows whose key actually changed are written.
#[utoipa::path(
    post,
    path = "/v1/admin/matching/rebuild-keys",
    tag = ADMIN_MATCHING_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "What the rebuild changed", body = KeyRebuildView),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
    )
)]
pub async fn rebuild_matching_keys(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<KeyRebuildView>> {
    user.require(Permission::MergeWrite).await?;
    let report = tankovault_db::repo::matching::rebuild_normalized_keys(
        &state.pool,
        tankovault_domain::normalize_title,
    )
    .await?;
    let view = KeyRebuildView {
        series_scanned: report.series_scanned,
        series_updated: report.series_updated,
        titles_scanned: report.titles_scanned,
        titles_updated: report.titles_updated,
        titles_deduplicated: report.titles_deduplicated,
    };
    audit(
        &state,
        &user,
        "matching.rebuild_keys",
        "-",
        &serde_json::to_value(view).unwrap_or_default(),
    )
    .await;
    Ok(Json(view))
}
