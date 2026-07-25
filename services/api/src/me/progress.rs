//! Read progress: whole-chapter and part frontiers, sync exclusion, targeted push.

use crate::error::ApiResult;
use crate::openapi::{ME_PROGRESS_TAG, ME_WATCHLIST_TAG};
use crate::state::{AppState, AuthUser};
use axum::Json;
use axum::extract::{Path, State};
use serde::{Deserialize, Serialize};
use tankovault_domain::{SeriesId, UserId};
use utoipa::ToSchema;

#[derive(Debug, Deserialize, ToSchema)]
pub struct ProgressUpdate {
    /// The whole-chapter frontier to set outright (design v2 §A.6 — renamed from
    /// `last_read_number`, same semantics).
    pub last_read_whole_number: f64,
}

/// Set the read-progress frontier
///
/// Set the whole-chapter frontier outright (design v2 §A.6).
#[utoipa::path(
    put,
    path = "/v1/me/progress/{series_id}",
    tag = ME_PROGRESS_TAG,
    params(("series_id" = SeriesId, Path, description = "Series id")),
    request_body = ProgressUpdate,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Acknowledged", body = serde_json::Value, example = json!({"ok": true})),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn put_progress(
    State(state): State<AppState>,
    user: AuthUser,
    Path(series_id): Path<SeriesId>,
    Json(body): Json<ProgressUpdate>,
) -> ApiResult<Json<serde_json::Value>> {
    tankovault_db::repo::tracking::progress_set(
        &state.pool,
        user.user_id,
        series_id,
        body.last_read_whole_number,
    )
    .await?;
    spawn_targeted_push(&state, user.user_id, series_id);
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ProgressDto {
    pub last_read_whole_number: f64,
    pub last_read_part_number: Option<f64>,
}

/// Get read progress
///
/// Both read frontiers for the series (design v2 §A.6).
#[utoipa::path(
    get,
    path = "/v1/me/progress/{series_id}",
    tag = ME_PROGRESS_TAG,
    params(("series_id" = SeriesId, Path, description = "Series id")),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Progress (defaults to zero when untracked)", body = ProgressDto),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn get_progress(
    State(state): State<AppState>,
    user: AuthUser,
    Path(series_id): Path<SeriesId>,
) -> ApiResult<Json<ProgressDto>> {
    let p = tankovault_db::repo::tracking::progress_get_full(&state.pool, user.user_id, series_id)
        .await?
        .unwrap_or_default();
    Ok(Json(ProgressDto {
        last_read_whole_number: p.last_read_whole_number,
        last_read_part_number: p.last_read_part_number,
    }))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ChapterRead {
    pub read: bool,
}

/// Mark a single chapter read or unread
///
/// Apply the §A.3 mark-read/mark-unread rule for one chapter number. Unmarking an older
/// (non-frontier) chapter retreats progress past it too; the client must confirm with the user
/// first in that case (design v2 §A.6).
#[utoipa::path(
    put,
    path = "/v1/me/progress/{series_id}/chapters/{number}",
    tag = ME_PROGRESS_TAG,
    params(
        ("series_id" = SeriesId, Path, description = "Series id"),
        ("number" = f64, Path, description = "Chapter number"),
    ),
    request_body = ChapterRead,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Acknowledged", body = serde_json::Value, example = json!({"ok": true})),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn put_chapter_progress(
    State(state): State<AppState>,
    user: AuthUser,
    Path((series_id, number)): Path<(SeriesId, f64)>,
    Json(body): Json<ChapterRead>,
) -> ApiResult<Json<serde_json::Value>> {
    if body.read {
        tankovault_db::repo::tracking::progress_mark_read(
            &state.pool,
            user.user_id,
            series_id,
            number,
        )
        .await?;
    } else {
        tankovault_db::repo::tracking::progress_mark_unread(
            &state.pool,
            user.user_id,
            series_id,
            number,
        )
        .await?;
    }
    spawn_targeted_push(&state, user.user_id, series_id);
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct MarkReadTo {
    pub number: f64,
}

/// Mark read up to a chapter
///
/// "Mark read to here" (design v2 §A.6); equivalent to marking `number` read.
#[utoipa::path(
    post,
    path = "/v1/me/progress/{series_id}/mark-read-to",
    tag = ME_PROGRESS_TAG,
    params(("series_id" = SeriesId, Path, description = "Series id")),
    request_body = MarkReadTo,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Acknowledged", body = serde_json::Value, example = json!({"ok": true})),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn mark_read_to(
    State(state): State<AppState>,
    user: AuthUser,
    Path(series_id): Path<SeriesId>,
    Json(body): Json<MarkReadTo>,
) -> ApiResult<Json<serde_json::Value>> {
    tankovault_db::repo::tracking::progress_mark_read(
        &state.pool,
        user.user_id,
        series_id,
        body.number,
    )
    .await?;
    spawn_targeted_push(&state, user.user_id, series_id);
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SyncExcluded {
    pub excluded: bool,
}

/// Set the per-series sync-exclusion flag
///
/// Set the blanket per-series sync-exclusion flag (design v2 §A.5).
#[utoipa::path(
    put,
    path = "/v1/me/watchlist/{series_id}/sync",
    tag = ME_WATCHLIST_TAG,
    params(("series_id" = SeriesId, Path, description = "Series id")),
    request_body = SyncExcluded,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Acknowledged", body = serde_json::Value, example = json!({"ok": true})),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn put_sync_excluded(
    State(state): State<AppState>,
    user: AuthUser,
    Path(series_id): Path<SeriesId>,
    Json(body): Json<SyncExcluded>,
) -> ApiResult<Json<serde_json::Value>> {
    tankovault_db::repo::tracking::set_sync_excluded(
        &state.pool,
        user.user_id,
        series_id,
        body.excluded,
    )
    .await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// Set a per-provider sync-exclusion override
///
/// Per-provider override of the blanket exclusion flag (design v2 §A.5).
#[utoipa::path(
    put,
    path = "/v1/me/watchlist/{series_id}/sync/{provider}",
    tag = ME_WATCHLIST_TAG,
    params(
        ("series_id" = SeriesId, Path, description = "Series id"),
        ("provider" = String, Path, description = "Provider slug"),
    ),
    request_body = SyncExcluded,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Acknowledged", body = serde_json::Value, example = json!({"ok": true})),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn put_sync_override(
    State(state): State<AppState>,
    user: AuthUser,
    Path((series_id, provider)): Path<(SeriesId, String)>,
    Json(body): Json<SyncExcluded>,
) -> ApiResult<Json<serde_json::Value>> {
    tankovault_db::repo::tracking::set_sync_override(
        &state.pool,
        user.user_id,
        series_id,
        &provider,
        body.excluded,
    )
    .await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// Best-effort background push of `series_id` to every provider `user_id` has linked. Mirrors
/// this codebase's existing "best-effort side effect" convention (notifier channels, the sync
/// engine's viewer-name lookup on link): logged on failure, never surfaced to the caller, never
/// blocks the response (design: immediate targeted push — marking a chapter/series read locally
/// reflects to `AniList` without a manual "Push" click).
pub(super) fn spawn_targeted_push(state: &AppState, user_id: UserId, series_id: SeriesId) {
    let http = state.http.clone();
    let sync_url = state.sync_url.clone();
    tokio::spawn(async move {
        let url = format!("{}/v1/sync/push-series", sync_url.trim_end_matches('/'));
        let body = serde_json::json!({ "user_id": user_id, "series_id": series_id });
        match http.post(url).json(&body).send().await {
            Ok(resp) if resp.status().is_success() => {}
            Ok(resp) => tracing::warn!(
                status = %resp.status(),
                %user_id,
                %series_id,
                "targeted sync push returned an error"
            ),
            Err(e) => tracing::warn!(
                error = %e,
                %user_id,
                %series_id,
                "targeted sync push unreachable"
            ),
        }
    });
}
