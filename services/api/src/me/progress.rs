//! Read progress: whole-chapter and part frontiers, sync exclusion, targeted push.

use super::watchlist::{BulkResult, WatchlistBulkIds, bulk_ids};
use crate::error::{ApiError, ApiResult};
use crate::openapi::{ME_PROGRESS_TAG, ME_WATCHLIST_TAG};
use crate::state::{AppState, AuthUser};
use axum::Json;
use axum::extract::{Path, State};
use serde::{Deserialize, Serialize};
use tankovault_domain::{Feature, SeriesId, UserId};
use utoipa::ToSchema;

#[derive(Debug, Deserialize, ToSchema)]
pub struct ProgressUpdate {
    /// The whole-chapter frontier to set outright (design v2 §A.6 — renamed from
    /// `last_read_number`, same semantics).
    pub last_read_whole_number: f64,
}

/// Set the read-progress frontier
///
/// Set the whole-chapter frontier outright (design v2 §A.6). A series the caller does not track
/// yet is added to their watchlist.
#[utoipa::path(
    put,
    path = "/v1/me/progress/{series_id}",
    tag = ME_PROGRESS_TAG,
    params(("series_id" = SeriesId, Path, description = "Series id")),
    request_body = ProgressUpdate,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Acknowledged", body = serde_json::Value, example = json!({"ok": true})),
        (status = 400, description = "the chapter number is not a storable value", body = crate::error::ProblemDetails),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn put_progress(
    State(state): State<AppState>,
    user: AuthUser,
    Path(series_id): Path<SeriesId>,
    Json(body): Json<ProgressUpdate>,
) -> ApiResult<Json<serde_json::Value>> {
    crate::auth::validate_chapter_number(body.last_read_whole_number)?;
    tankovault_db::repo::tracking::progress_set(
        &state.pool,
        user.user_id,
        series_id,
        body.last_read_whole_number,
    )
    .await?;
    track_read_series(&state, user.user_id, series_id).await?;
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
///
/// Marking read also adds a series the caller does not track yet to their watchlist; marking
/// unread never removes it.
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
        (status = 400, description = "the chapter number is not a storable value", body = crate::error::ProblemDetails),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn put_chapter_progress(
    State(state): State<AppState>,
    user: AuthUser,
    Path((series_id, number)): Path<(SeriesId, f64)>,
    Json(body): Json<ChapterRead>,
) -> ApiResult<Json<serde_json::Value>> {
    // The number is a path *segment* here, and `f64::from_str` reads "NaN" and "inf".
    crate::auth::validate_chapter_number(number)?;
    if body.read {
        tankovault_db::repo::tracking::progress_mark_read(
            &state.pool,
            user.user_id,
            series_id,
            number,
        )
        .await?;
        track_read_series(&state, user.user_id, series_id).await?;
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
/// "Mark read to here" (design v2 §A.6); equivalent to marking `number` read, watchlist
/// side effect included.
#[utoipa::path(
    post,
    path = "/v1/me/progress/{series_id}/mark-read-to",
    tag = ME_PROGRESS_TAG,
    params(("series_id" = SeriesId, Path, description = "Series id")),
    request_body = MarkReadTo,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Acknowledged", body = serde_json::Value, example = json!({"ok": true})),
        (status = 400, description = "the chapter number is not a storable value", body = crate::error::ProblemDetails),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn mark_read_to(
    State(state): State<AppState>,
    user: AuthUser,
    Path(series_id): Path<SeriesId>,
    Json(body): Json<MarkReadTo>,
) -> ApiResult<Json<serde_json::Value>> {
    crate::auth::validate_chapter_number(body.number)?;
    tankovault_db::repo::tracking::progress_mark_read(
        &state.pool,
        user.user_id,
        series_id,
        body.number,
    )
    .await?;
    track_read_series(&state, user.user_id, series_id).await?;
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
///
/// The flag lives on the watchlist entry, so the series must already be tracked. It answers
/// `404` when it is not, rather than the `{"ok": true}` it used to answer unconditionally
/// (OPS-2.2d): this decides whether the caller's reading progress is pushed to an external
/// provider, and a privacy setting that reports success without persisting is worse than one
/// that refuses.
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
        (status = 404, description = "The caller does not track this series", body = crate::error::ProblemDetails),
    )
)]
pub async fn put_sync_excluded(
    State(state): State<AppState>,
    user: AuthUser,
    Path(series_id): Path<SeriesId>,
    Json(body): Json<SyncExcluded>,
) -> ApiResult<Json<serde_json::Value>> {
    let written = tankovault_db::repo::tracking::set_sync_excluded(
        &state.pool,
        user.user_id,
        series_id,
        body.excluded,
    )
    .await?;
    if !written {
        return Err(ApiError::NotFound);
    }
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

/// Mark every chapter read across many series
///
/// Backs the Watchlist's `Mark group read`: each listed series' progress advances to its
/// highest known chapter. Ids the caller does not track come back in `skipped`.
///
/// The frontier only moves forward, so pressing this twice is a no-op the second time, and a
/// series whose catalogue has shrunk is not rewound.
#[utoipa::path(
    post,
    path = "/v1/me/progress/bulk-read",
    tag = ME_PROGRESS_TAG,
    request_body = WatchlistBulkIds,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Per-id outcome", body = BulkResult),
        (status = 400, description = "empty or oversized id list", body = crate::error::ProblemDetails),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn bulk_mark_read(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<WatchlistBulkIds>,
) -> ApiResult<Json<BulkResult>> {
    let ids = bulk_ids(&body.series_ids)?;
    let applied =
        tankovault_db::repo::tracking::progress_bulk_mark_all_read(&state.pool, user.user_id, &ids)
            .await?;
    spawn_targeted_push_many(&state, user.user_id, applied.clone());
    Ok(Json(BulkResult::new(&body.series_ids, applied)))
}

/// Put `series_id` on the caller's watchlist if it is not there already, because reading a
/// chapter is itself the statement that they follow the series. An entry that already exists is
/// left untouched, status and notify flag included.
///
/// Runs *after* the progress write, and its failure is reported rather than swallowed: the
/// mark-read endpoints are idempotent, so a caller that retries a `500` lands both writes,
/// whereas tracking first would leave a watchlist entry behind for a progress write that never
/// happened.
///
/// Gated on [`Feature::TrackingWatchlist`], which is independent of the [`Feature`] guarding
/// these routes: with the watchlist switched off, marking read must keep working *and* must not
/// write to a surface the operator disabled.
async fn track_read_series(
    state: &AppState,
    user_id: UserId,
    series_id: SeriesId,
) -> ApiResult<()> {
    if !state.features.is_enabled(Feature::TrackingWatchlist) {
        return Ok(());
    }
    tankovault_db::repo::tracking::watchlist_track_if_absent(&state.pool, user_id, series_id)
        .await?;
    Ok(())
}

/// Best-effort background push of every id, sequentially in one task rather than one task per
/// id — a 200-item bulk mark-read would otherwise burst 200 concurrent pushes at
/// `services/sync` and the third-party API behind it.
pub(super) fn spawn_targeted_push_many(
    state: &AppState,
    user_id: UserId,
    series_ids: Vec<SeriesId>,
) {
    if series_ids.is_empty() {
        return;
    }
    let state = state.clone();
    // Crosses into `sync` over HTTP, so losing the hub here would break the trace at a
    // service boundary rather than merely inside this one.
    tokio::spawn(tankovault_service::in_current_trace(async move {
        for series_id in series_ids {
            push_series(&state, user_id, series_id).await;
        }
    }));
}

/// Best-effort background push of `series_id` to every linked provider; logged on failure,
/// never surfaced to the caller (design: local reads reflect to `AniList` without a manual
/// "Push" click).
pub(super) fn spawn_targeted_push(state: &AppState, user_id: UserId, series_id: SeriesId) {
    if !push_enabled(state) {
        return;
    }
    let state = state.clone();
    tokio::spawn(tankovault_service::in_current_trace(async move {
        push_series(&state, user_id, series_id).await;
    }));
}

/// Whether a targeted push should be attempted, gated on both [`Feature::SyncAutoPush`] and
/// [`Feature::SyncExternal`]. Checked here rather than in the route-feature table since marking
/// a chapter read must keep working regardless — only whether it reaches a third party varies.
fn push_enabled(state: &AppState) -> bool {
    state.features.is_enabled(Feature::SyncExternal)
        && state.features.is_enabled(Feature::SyncAutoPush)
}

/// One push. Logged on failure, never surfaced — the caller's write already succeeded locally,
/// so failing their request over an unreachable third party would report the wrong thing.
async fn push_series(state: &AppState, user_id: UserId, series_id: SeriesId) {
    if !push_enabled(state) {
        return;
    }
    let body = serde_json::json!({ "user_id": user_id, "series_id": series_id });
    let request = state
        .sync
        .request(reqwest::Method::POST, "/v1/sync/push-series")
        .json(&body);
    match request.send().await {
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
}
