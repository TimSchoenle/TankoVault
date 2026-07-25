//! The authenticated user's watchlist: listing, upsert, removal.

use super::progress::spawn_targeted_push;
use crate::error::ApiResult;
use crate::openapi::ME_WATCHLIST_TAG;
use crate::state::{AppState, AuthUser};
use axum::Json;
use axum::extract::{Path, State};
use serde::{Deserialize, Serialize};
use tankovault_domain::{SeriesId, WatchStatus};
use utoipa::ToSchema;

#[derive(Debug, Serialize, ToSchema)]
pub struct WatchlistItem {
    pub series_id: SeriesId,
    pub status: WatchStatus,
    pub notify: bool,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub added_at: time::OffsetDateTime,
    /// Embedded series title so the Watchlist board renders without a per-card detail
    /// fetch (frontend §9.3, kills the N+1).
    pub series_title: String,
    pub cover_url: Option<String>,
    /// The user's last-read chapter number, if any.
    pub last_read_number: Option<f64>,
    /// Unread chapters above the user's progress.
    pub unread: i64,
    /// Whether this series is opted out of external sync (design v2 §A.5).
    pub sync_excluded: bool,
}

/// Get the watchlist
///
/// The user's watchlist with embedded title/cover/progress (frontend §9.3). Extra fields are
/// additive; older clients ignore them.
#[utoipa::path(
    get,
    path = "/v1/me/watchlist",
    tag = ME_WATCHLIST_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "The caller's watchlist", body = Vec<WatchlistItem>),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn watchlist(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<Vec<WatchlistItem>>> {
    let cards =
        tankovault_db::repo::tracking::watchlist_detailed(&state.pool, user.user_id).await?;
    let out = cards
        .into_iter()
        .map(|c| WatchlistItem {
            series_id: c.series_id,
            status: c.status,
            notify: c.notify,
            added_at: c.added_at,
            series_title: c.series_title,
            cover_url: c.cover_url,
            last_read_number: c.last_read_number,
            unread: c.unread,
            sync_excluded: c.sync_excluded,
        })
        .collect();
    Ok(Json(out))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct WatchlistUpsert {
    #[serde(default)]
    pub status: WatchStatus,
    #[serde(default = "default_true")]
    pub notify: bool,
}

fn default_true() -> bool {
    true
}

/// Add or update a watchlist entry
#[utoipa::path(
    put,
    path = "/v1/me/watchlist/{series_id}",
    tag = ME_WATCHLIST_TAG,
    params(("series_id" = SeriesId, Path, description = "Series id")),
    request_body = WatchlistUpsert,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Acknowledged", body = serde_json::Value, example = json!({"ok": true})),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn put_watchlist(
    State(state): State<AppState>,
    user: AuthUser,
    Path(series_id): Path<SeriesId>,
    Json(body): Json<WatchlistUpsert>,
) -> ApiResult<Json<serde_json::Value>> {
    tankovault_db::repo::tracking::watchlist_upsert(
        &state.pool,
        user.user_id,
        series_id,
        body.status,
        body.notify,
    )
    .await?;
    spawn_targeted_push(&state, user.user_id, series_id);
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// Remove a watchlist entry
#[utoipa::path(
    delete,
    path = "/v1/me/watchlist/{series_id}",
    tag = ME_WATCHLIST_TAG,
    params(("series_id" = SeriesId, Path, description = "Series id")),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Acknowledged", body = serde_json::Value, example = json!({"ok": true})),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn delete_watchlist(
    State(state): State<AppState>,
    user: AuthUser,
    Path(series_id): Path<SeriesId>,
) -> ApiResult<Json<serde_json::Value>> {
    tankovault_db::repo::tracking::watchlist_remove(&state.pool, user.user_id, series_id).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}
