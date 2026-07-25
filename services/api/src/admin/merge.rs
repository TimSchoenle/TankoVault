//! The duplicate-series merge queue: candidates, merges, dismissals.

use crate::audit::audit;
use crate::error::ApiResult;
use crate::openapi::ADMIN_MATCHING_TAG;
use crate::state::{AppState, AuthUser};
use axum::Json;
use axum::extract::State;
use serde::Deserialize;
use tankovault_db::repo::matching::MergeCandidateView;
use tankovault_domain::{SeriesId, UserRole};
use utoipa::ToSchema;
use uuid::Uuid;

/// List merge candidates
///
/// The canonicalisation review queue (design §10).
#[utoipa::path(
    get,
    path = "/v1/admin/merge-candidates",
    tag = ADMIN_MATCHING_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Up to 200 open merge candidates", body = Vec<MergeCandidateView>),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller must have at least the operator role", body = crate::error::ProblemDetails),
    )
)]
pub async fn list_merge_candidates(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<Vec<MergeCandidateView>>> {
    user.require(UserRole::Operator).await?;
    Ok(Json(
        tankovault_db::repo::matching::list_open_merge_candidates(&state.pool, 200).await?,
    ))
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
/// Transactional re-parent + title/tag union (design §10).
#[utoipa::path(
    post,
    path = "/v1/admin/series/merge",
    tag = ADMIN_MATCHING_TAG,
    request_body = MergeRequest,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Acknowledged", body = serde_json::Value, example = json!({"ok": true})),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller must have at least the operator role", body = crate::error::ProblemDetails),
        (status = 404, description = "One or both series not found", body = crate::error::ProblemDetails),
        (status = 409, description = "`keep` and `merge` are the same series", body = crate::error::ProblemDetails),
    )
)]
pub async fn merge_series(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<MergeRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    user.require(UserRole::Operator).await?;
    tankovault_db::repo::matching::merge_series(
        &state.pool,
        req.keep,
        req.merge,
        Some(user.user_id),
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
/// Operator judged the two works distinct.
#[utoipa::path(
    post,
    path = "/v1/admin/merge-candidates/dismiss",
    tag = ADMIN_MATCHING_TAG,
    request_body = DismissRequest,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Whether a candidate was actually dismissed", body = serde_json::Value, example = json!({"dismissed": true})),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller must have at least the operator role", body = crate::error::ProblemDetails),
    )
)]
pub async fn dismiss_merge_candidate(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<DismissRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    user.require(UserRole::Operator).await?;
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
