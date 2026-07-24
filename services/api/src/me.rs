//! Authenticated user tracking: watchlist, read progress, notifications. Ownership is
//! enforced implicitly — every query is scoped to the token's `user_id`.

use crate::error::{ApiError, ApiResult};
use crate::state::{AppState, AuthUser};
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::fmt::Write as _;
use tankovault_contracts::UserNotification;
use tankovault_domain::{SeriesId, UserId, WatchStatus, resolve_link};
use time::OffsetDateTime;
use tokio_stream::StreamExt as _;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::openapi::{
    ME_ACCOUNT_TAG, ME_DASHBOARD_TAG, ME_NOTIFICATIONS_TAG, ME_PROGRESS_TAG, ME_SYNC_TAG,
    ME_WATCHLIST_TAG,
};

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
fn spawn_targeted_push(state: &AppState, user_id: UserId, series_id: SeriesId) {
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

/// List notifications
#[utoipa::path(
    get,
    path = "/v1/me/notifications",
    tag = ME_NOTIFICATIONS_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Up to 100 most recent notifications", body = serde_json::Value),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn notifications(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<serde_json::Value>> {
    let list =
        tankovault_db::repo::tracking::notifications_list(&state.pool, user.user_id, 100).await?;
    Ok(Json(serde_json::to_value(list).unwrap_or_default()))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct MarkRead {
    pub ids: Vec<Uuid>,
}

/// Mark notifications read
#[utoipa::path(
    post,
    path = "/v1/me/notifications/read",
    tag = ME_NOTIFICATIONS_TAG,
    request_body = MarkRead,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Number of notifications marked read", body = serde_json::Value, example = json!({"marked": 3})),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn mark_read(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<MarkRead>,
) -> ApiResult<Json<serde_json::Value>> {
    let n = tankovault_db::repo::tracking::notifications_mark_read(
        &state.pool,
        user.user_id,
        &body.ids,
    )
    .await?;
    Ok(Json(serde_json::json!({ "marked": n })))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct FeedEntry {
    pub series_id: SeriesId,
    pub series_title: String,
    pub chapter_number: f64,
    pub chapter_title: Option<String>,
    pub provider_slug: String,
    /// Ready-to-open absolute URL, resolved from the provider base + relative path.
    pub url: String,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub discovered_at: OffsetDateTime,
}

/// Get the unread-chapters feed
///
/// Unread chapters across the watchlist (the reading dashboard).
#[utoipa::path(
    get,
    path = "/v1/me/feed",
    tag = ME_DASHBOARD_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Up to 100 most recent unread chapters", body = Vec<FeedEntry>),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn feed(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<Vec<FeedEntry>>> {
    let items = tankovault_db::repo::tracking::feed(&state.pool, user.user_id, 100).await?;
    let out = items
        .into_iter()
        .map(|i| {
            Ok(FeedEntry {
                series_id: i.series_id,
                series_title: i.series_title,
                chapter_number: i.chapter_number,
                chapter_title: i.chapter_title,
                provider_slug: i.provider_slug,
                url: resolve_link(&i.base_url, &i.chapter_path).map_err(|_| ApiError::Internal)?,
                discovered_at: i.discovered_at,
            })
        })
        .collect::<ApiResult<Vec<_>>>()?;
    Ok(Json(out))
}

// ---------------------------------------------------------------------------
// Reading dashboard (frontend §9.3)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, ToSchema)]
pub struct ContinueItem {
    pub series_id: SeriesId,
    pub series_title: String,
    pub cover_url: Option<String>,
    pub last_read_number: f64,
    /// The lowest unread chapter number above the user's progress, if any.
    pub next_number: Option<f64>,
    pub unread: i64,
}

/// Get continue-reading cards
///
/// Continue-reading cards for Home / the Series CTA (frontend §9.3): tracked, in-progress
/// series that have unread chapters, freshest activity first.
#[utoipa::path(
    get,
    path = "/v1/me/continue",
    tag = ME_DASHBOARD_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Up to 24 continue-reading cards", body = Vec<ContinueItem>),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn continue_reading(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<Vec<ContinueItem>>> {
    let cards =
        tankovault_db::repo::tracking::continue_reading(&state.pool, user.user_id, 24).await?;
    let out = cards
        .into_iter()
        .map(|c| ContinueItem {
            series_id: c.series_id,
            series_title: c.series_title,
            cover_url: c.cover_url,
            last_read_number: c.last_read_number,
            next_number: c.next_number,
            unread: c.unread,
        })
        .collect();
    Ok(Json(out))
}

/// Get "because you read" recommendations
///
/// *Stub*: unwatched series sharing tags with the user's list (frontend §9.3). Falls back to
/// the most-recent catalog when the user has no tagged watchlist yet, so the shelf is never
/// empty for signed-in users.
#[utoipa::path(
    get,
    path = "/v1/me/recommendations",
    tag = ME_DASHBOARD_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Up to 12 recommended series", body = Vec<crate::series::SeriesSummary>),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn recommendations(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<Vec<crate::series::SeriesSummary>>> {
    let mut items =
        tankovault_db::repo::tracking::recommendations(&state.pool, user.user_id, 12).await?;
    if items.is_empty() {
        items = tankovault_db::repo::catalog::list_series(&state.pool, None, 12).await?;
    }
    let out = items
        .into_iter()
        .map(|it| crate::series::SeriesSummary {
            id: it.series.id,
            title: it.series.canonical_title,
            cover_url: it.series.cover_url,
            content_type: it.series.content_type,
            status: it.series.status,
            source_count: it.source_count,
        })
        .collect();
    Ok(Json(out))
}

/// Get lifetime tracking stats
///
/// *Stub*: lifetime tracking stats for the Home / Profile headline (frontend §9.3). See
/// [`tankovault_db::repo::tracking::MeStats`] for the honest definition of `chapters_read` and
/// why no "streak" is returned.
#[utoipa::path(
    get,
    path = "/v1/me/stats",
    tag = ME_DASHBOARD_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Lifetime stats", body = tankovault_db::repo::tracking::MeStats),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn stats(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<tankovault_db::repo::tracking::MeStats>> {
    Ok(Json(
        tankovault_db::repo::tracking::me_stats(&state.pool, user.user_id).await?,
    ))
}

// ---------------------------------------------------------------------------
// Account settings (frontend §9.4)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, ToSchema)]
pub struct ProfileUpdate {
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ProfileDto {
    pub id: uuid::Uuid,
    pub email: String,
    pub username: String,
    pub role: String,
}

/// Update the profile
///
/// Update the caller's username and/or email (frontend §9.4). A duplicate email/username
/// surfaces as `409 Conflict`.
#[utoipa::path(
    patch,
    path = "/v1/me/profile",
    tag = ME_ACCOUNT_TAG,
    request_body = ProfileUpdate,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Updated profile", body = ProfileDto),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 409, description = "Email or username already taken", body = crate::error::ProblemDetails),
    )
)]
pub async fn patch_profile(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<ProfileUpdate>,
) -> ApiResult<Json<ProfileDto>> {
    let username = body
        .username
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let email = body
        .email
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let updated =
        tankovault_db::repo::users::update_profile(&state.pool, user.user_id, username, email)
            .await?;
    Ok(Json(ProfileDto {
        id: updated.id.as_uuid(),
        email: updated.email,
        username: updated.username,
        role: updated.role.as_str().to_owned(),
    }))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SessionDto {
    pub id: String,
    pub family_id: String,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub expires_at: OffsetDateTime,
}

/// List active sessions
///
/// The caller's active login sessions (frontend §9.4).
#[utoipa::path(
    get,
    path = "/v1/me/sessions",
    tag = ME_ACCOUNT_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Active sessions", body = Vec<SessionDto>),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn sessions(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<Vec<SessionDto>>> {
    let list = tankovault_db::repo::users::list_sessions(&state.pool, user.user_id).await?;
    let out = list
        .into_iter()
        .map(|s| SessionDto {
            id: s.id.to_string(),
            family_id: s.family_id.to_string(),
            created_at: s.created_at,
            expires_at: s.expires_at,
        })
        .collect();
    Ok(Json(out))
}

/// Revoke a session
///
/// Revoke one of the caller's own sessions (frontend §9.4). Scoped to ownership; a
/// foreign/unknown id yields `404`.
#[utoipa::path(
    delete,
    path = "/v1/me/sessions/{id}",
    tag = ME_ACCOUNT_TAG,
    params(("id" = uuid::Uuid, Path, description = "Session id")),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Revoked", body = serde_json::Value, example = json!({"revoked": 1})),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 404, description = "No such session for this caller", body = crate::error::ProblemDetails),
    )
)]
pub async fn delete_session(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let revoked = tankovault_db::repo::users::revoke_session(&state.pool, user.user_id, id).await?;
    if revoked == 0 {
        return Err(ApiError::NotFound);
    }
    Ok(Json(serde_json::json!({ "revoked": revoked })))
}

/// Get notification preferences
///
/// The caller's notification preferences JSON (frontend §9.4). `{}` means "product defaults".
#[utoipa::path(
    get,
    path = "/v1/me/notification-prefs",
    tag = ME_NOTIFICATIONS_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Product-defined free-form preferences JSON", body = serde_json::Value),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn notification_prefs(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<serde_json::Value>> {
    Ok(Json(
        tankovault_db::repo::users::get_notification_prefs(&state.pool, user.user_id).await?,
    ))
}

/// Replace notification preferences
///
/// Replace the caller's notification preferences (frontend §9.4). The body is stored verbatim
/// as an open JSON document.
#[utoipa::path(
    put,
    path = "/v1/me/notification-prefs",
    tag = ME_NOTIFICATIONS_TAG,
    request_body = serde_json::Value,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "The stored preferences, echoed back", body = serde_json::Value),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn put_notification_prefs(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<serde_json::Value>,
) -> ApiResult<Json<serde_json::Value>> {
    tankovault_db::repo::users::set_notification_prefs(&state.pool, user.user_id, &body).await?;
    Ok(Json(body))
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct StreamQuery {
    /// Access token, passed as a query parameter because the browser `EventSource` API
    /// cannot attach an `Authorization` header (design §17.4). It is verified exactly like
    /// a `Bearer` token and never logged.
    pub access_token: String,
}

/// Live notification stream
///
/// Server-Sent Events of live per-user notifications (design §14, §17.4). Authenticated by the
/// `access_token` query parameter (not the `Authorization` header — `EventSource` cannot set
/// it), it subscribes to the user's core-NATS subject and relays each `UserNotification` as a
/// `notification` SSE event, with a periodic keep-alive comment so proxies keep the connection
/// open. Ownership is implicit: the subscription is scoped to the token's own `user_id`.
#[utoipa::path(
    get,
    path = "/v1/me/stream",
    tag = ME_NOTIFICATIONS_TAG,
    params(StreamQuery),
    responses(
        (status = 200, description = "SSE stream of `notification` events", content_type = "text/event-stream"),
        (status = 401, description = "missing or invalid access_token", body = crate::error::ProblemDetails),
        (status = 503, description = "the live notification stream is temporarily unavailable", body = crate::error::ProblemDetails),
    )
)]
pub async fn stream(
    State(state): State<AppState>,
    Query(q): Query<StreamQuery>,
) -> ApiResult<Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>>> {
    let claims = tankovault_auth::verify_access_token(&state.jwt_secret, &q.access_token)?;
    let user_id = claims.user_id().ok_or(ApiError::Unauthorized)?;

    let bus = state.bus.clone().ok_or(ApiError::Unavailable)?;
    let subscriber = bus
        .subscribe_user_notifications(user_id.as_uuid())
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to open notification subscription");
            ApiError::Internal
        })?;

    let events = subscriber.map(|msg| {
        let event = match serde_json::from_slice::<UserNotification>(&msg.payload) {
            Ok(notification) => Event::default()
                .event("notification")
                .json_data(&notification)
                .unwrap_or_else(|_| Event::default().comment("serialize error")),
            Err(_) => Event::default().comment("undecodable notification"),
        };
        Ok(event)
    });

    Ok(Sse::new(events).keep_alive(KeepAlive::default()))
}

/// List sync providers
///
/// The registered external providers (design: generalized multi-provider sync). Drives the
/// Account "Sync & integrations" panel, which renders one card per entry instead of a single
/// hardcoded `AniList` block. Response shape is defined by the sync service and forwarded
/// verbatim; not tracked here.
#[utoipa::path(
    get,
    path = "/v1/me/sync/providers",
    tag = ME_SYNC_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Providers, forwarded from the sync service"),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn sync_providers(
    State(state): State<AppState>,
    _user: AuthUser,
) -> ApiResult<Json<serde_json::Value>> {
    let url = format!("{}/v1/sync/providers", state.sync_url.trim_end_matches('/'));
    let resp = state.http.get(url).send().await.map_err(|e| {
        tracing::error!(error = %e, "sync service unreachable");
        ApiError::Internal
    })?;
    if !resp.status().is_success() {
        return Err(ApiError::Internal);
    }
    Ok(Json(resp.json().await.map_err(|_| ApiError::Internal)?))
}

/// Get a provider's OAuth consent URL
///
/// Returns the provider's consent URL (proxied). Response shape is defined by the sync service
/// and forwarded verbatim; not tracked here.
#[utoipa::path(
    get,
    path = "/v1/me/sync/{provider}/authorize",
    tag = ME_SYNC_TAG,
    params(("provider" = String, Path, description = "Provider slug")),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Consent URL, forwarded from the sync service"),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn sync_authorize_url(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(provider): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let url = format!(
        "{}/v1/sync/{provider}/authorize-url",
        state.sync_url.trim_end_matches('/')
    );
    let resp = state.http.get(url).send().await.map_err(|e| {
        tracing::error!(error = %e, "sync service unreachable");
        ApiError::Internal
    })?;
    if !resp.status().is_success() {
        return Err(ApiError::Internal);
    }
    Ok(Json(resp.json().await.map_err(|_| ApiError::Internal)?))
}

/// Get link status for a provider
///
/// Whether the caller has a linked account at `provider`, plus the connected display name and
/// most recent sync time (Sync & integrations panel, header pill, Series tracking card).
/// Always `200`; an unlinked account reads `{ "linked": false }`. Response shape is defined by
/// the sync service and forwarded verbatim; not tracked here.
#[utoipa::path(
    get,
    path = "/v1/me/sync/{provider}/status",
    tag = ME_SYNC_TAG,
    params(("provider" = String, Path, description = "Provider slug")),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Link status, forwarded from the sync service"),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn sync_status(
    State(state): State<AppState>,
    user: AuthUser,
    Path(provider): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let url = format!(
        "{}/v1/sync/{provider}/status/{}",
        state.sync_url.trim_end_matches('/'),
        user.user_id.as_uuid()
    );
    let resp = state.http.get(url).send().await.map_err(|e| {
        tracing::error!(error = %e, "sync service unreachable");
        ApiError::Internal
    })?;
    if !resp.status().is_success() {
        return Err(ApiError::Internal);
    }
    Ok(Json(resp.json().await.map_err(|_| ApiError::Internal)?))
}

/// Unlink a provider
///
/// Unlink the caller's account at `provider`. Response shape is defined by the sync service
/// and forwarded verbatim; not tracked here.
#[utoipa::path(
    delete,
    path = "/v1/me/sync/{provider}",
    tag = ME_SYNC_TAG,
    params(("provider" = String, Path, description = "Provider slug")),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Unlinked, forwarded from the sync service"),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn sync_disconnect(
    State(state): State<AppState>,
    user: AuthUser,
    Path(provider): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let url = format!(
        "{}/v1/sync/{provider}/link",
        state.sync_url.trim_end_matches('/')
    );
    let resp = state
        .http
        .delete(url)
        .json(&serde_json::json!({ "user_id": user.user_id }))
        .send()
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "sync service unreachable");
            ApiError::Internal
        })?;
    if !resp.status().is_success() {
        return Err(ApiError::Internal);
    }
    Ok(Json(resp.json().await.map_err(|_| ApiError::Internal)?))
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct AniListCallback {
    pub code: String,
}

/// Complete an OAuth link
///
/// Exchanges the authorization `code` and links the caller's account at `provider`. Response
/// shape is defined by the sync service and forwarded verbatim; not tracked here.
#[utoipa::path(
    get,
    path = "/v1/me/sync/{provider}/callback",
    tag = ME_SYNC_TAG,
    params(("provider" = String, Path, description = "Provider slug"), AniListCallback),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Linked, forwarded from the sync service"),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 404, description = "Unknown provider", body = crate::error::ProblemDetails),
        (status = 409, description = "Account not linked", body = crate::error::ProblemDetails),
    )
)]
pub async fn sync_callback(
    State(state): State<AppState>,
    user: AuthUser,
    Path(provider): Path<String>,
    Query(q): Query<AniListCallback>,
) -> ApiResult<Json<serde_json::Value>> {
    sync_proxy(
        &state,
        &format!("/v1/sync/{provider}/link"),
        serde_json::json!({ "user_id": user.user_id, "code": q.code }),
    )
    .await
}

#[derive(Debug, Deserialize, Default, ToSchema)]
pub struct SyncOpts {
    /// `local_wins` | `remote_wins` | `newest_wins`; omitted uses the service default.
    #[serde(default)]
    pub policy: Option<String>,
}

/// Push local state to a provider
///
/// Reflect local watchlist/progress to `provider` (bulk, full-reconciliation walk — see
/// `spawn_targeted_push` for the fast per-series path used automatically when marking a
/// chapter/series read). Response shape is defined by the sync service and forwarded
/// verbatim; not tracked here.
#[utoipa::path(
    post,
    path = "/v1/me/sync/{provider}/push",
    tag = ME_SYNC_TAG,
    params(("provider" = String, Path, description = "Provider slug")),
    request_body(content = Option<SyncOpts>, description = "Optional sync options; omitted body uses the service default"),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Pushed, forwarded from the sync service"),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 409, description = "Account not linked", body = crate::error::ProblemDetails),
    )
)]
pub async fn sync_push(
    State(state): State<AppState>,
    user: AuthUser,
    Path(provider): Path<String>,
    body: Option<Json<SyncOpts>>,
) -> ApiResult<Json<serde_json::Value>> {
    let opts = body.map(|b| b.0).unwrap_or_default();
    sync_proxy(
        &state,
        &format!("/v1/sync/{provider}/push"),
        serde_json::json!({ "user_id": user.user_id, "policy": opts.policy }),
    )
    .await
}

/// Pull a provider's list into local state
///
/// Import `provider`'s list into the local watchlist. Response shape is defined by the sync
/// service and forwarded verbatim; not tracked here.
#[utoipa::path(
    post,
    path = "/v1/me/sync/{provider}/pull",
    tag = ME_SYNC_TAG,
    params(("provider" = String, Path, description = "Provider slug")),
    request_body(content = Option<SyncOpts>, description = "Optional sync options; omitted body uses the service default"),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Pulled, forwarded from the sync service"),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 409, description = "Account not linked", body = crate::error::ProblemDetails),
    )
)]
pub async fn sync_pull(
    State(state): State<AppState>,
    user: AuthUser,
    Path(provider): Path<String>,
    body: Option<Json<SyncOpts>>,
) -> ApiResult<Json<serde_json::Value>> {
    let opts = body.map(|b| b.0).unwrap_or_default();
    sync_proxy(
        &state,
        &format!("/v1/sync/{provider}/pull"),
        serde_json::json!({ "user_id": user.user_id, "policy": opts.policy }),
    )
    .await
}

/// Get automatic-sync settings
///
/// The caller's automatic-sync settings (design v2 §B.6). Response shape is defined by the
/// sync service and forwarded verbatim; not tracked here.
#[utoipa::path(
    get,
    path = "/v1/me/sync/{provider}/settings",
    tag = ME_SYNC_TAG,
    params(("provider" = String, Path, description = "Provider slug")),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Settings, forwarded from the sync service"),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 404, description = "No settings for this provider", body = crate::error::ProblemDetails),
    )
)]
pub async fn sync_settings(
    State(state): State<AppState>,
    user: AuthUser,
    Path(provider): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    sync_get(
        &state,
        &format!("/v1/sync/{provider}/settings/{}", user.user_id.as_uuid()),
    )
    .await
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SyncSettingsPatch {
    #[serde(default)]
    pub auto_sync_enabled: Option<bool>,
    #[serde(default)]
    pub conflict_policy: Option<String>,
}

/// Update automatic-sync settings
///
/// Update automatic sync + conflict policy (design v2 §B.6).
#[utoipa::path(
    patch,
    path = "/v1/me/sync/{provider}/settings",
    tag = ME_SYNC_TAG,
    params(("provider" = String, Path, description = "Provider slug")),
    request_body = SyncSettingsPatch,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Acknowledged", body = serde_json::Value, example = json!({"ok": true})),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn sync_settings_patch(
    State(state): State<AppState>,
    user: AuthUser,
    Path(provider): Path<String>,
    Json(body): Json<SyncSettingsPatch>,
) -> ApiResult<Json<serde_json::Value>> {
    let url = format!(
        "{}/v1/sync/{provider}/settings/{}",
        state.sync_url.trim_end_matches('/'),
        user.user_id.as_uuid()
    );
    let payload = serde_json::json!({
        "user_id": user.user_id,
        "auto_sync_enabled": body.auto_sync_enabled,
        "conflict_policy": body.conflict_policy,
    });
    let resp = state
        .http
        .patch(url)
        .json(&payload)
        .send()
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "sync service unreachable");
            ApiError::Internal
        })?;
    if !resp.status().is_success() {
        return Err(ApiError::Internal);
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// List pending sync conflicts
///
/// The caller's pending conflicts across all providers (§B.6). Response shape is defined by
/// the sync service and forwarded verbatim; not tracked here.
#[utoipa::path(
    get,
    path = "/v1/me/sync/conflicts",
    tag = ME_SYNC_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Pending conflicts, forwarded from the sync service"),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn sync_conflicts(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<serde_json::Value>> {
    sync_get(
        &state,
        &format!("/v1/sync/conflicts/{}", user.user_id.as_uuid()),
    )
    .await
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ResolveConflict {
    pub resolution: String,
}

/// Resolve a sync conflict
///
/// Apply the caller's chosen resolution (§B.6). Response shape is defined by the sync service
/// and forwarded verbatim; not tracked here.
#[utoipa::path(
    post,
    path = "/v1/me/sync/conflicts/{id}/resolve",
    tag = ME_SYNC_TAG,
    params(("id" = uuid::Uuid, Path, description = "Conflict id")),
    request_body = ResolveConflict,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Resolved, forwarded from the sync service"),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 404, description = "No such conflict", body = crate::error::ProblemDetails),
    )
)]
pub async fn sync_resolve_conflict(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<ResolveConflict>,
) -> ApiResult<Json<serde_json::Value>> {
    sync_proxy(
        &state,
        &format!("/v1/sync/conflicts/{id}/resolve"),
        serde_json::json!({ "user_id": user.user_id, "resolution": body.resolution }),
    )
    .await
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct HistoryParams {
    #[serde(default)]
    pub series_id: Option<Uuid>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub page: Option<i64>,
}

/// Get sync history
///
/// A page of the caller's sync history (§B.6). Response shape is defined by the sync service
/// and forwarded verbatim; not tracked here.
#[utoipa::path(
    get,
    path = "/v1/me/sync/history",
    tag = ME_SYNC_TAG,
    params(HistoryParams),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "A page of sync history, forwarded from the sync service"),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn sync_history(
    State(state): State<AppState>,
    user: AuthUser,
    Query(q): Query<HistoryParams>,
) -> ApiResult<Json<serde_json::Value>> {
    let mut path = format!("/v1/sync/history/{}?", user.user_id.as_uuid());
    if let Some(s) = q.series_id {
        let _ = write!(path, "series_id={s}&");
    }
    if let Some(p) = &q.provider {
        let _ = write!(path, "provider={p}&");
    }
    let _ = write!(path, "page={}", q.page.unwrap_or(0));
    sync_get(&state, &path).await
}

/// GET a JSON body from the sync service, mapping upstream errors like `sync_proxy`.
pub(crate) async fn sync_get(state: &AppState, path: &str) -> ApiResult<Json<serde_json::Value>> {
    let url = format!(
        "{}/{}",
        state.sync_url.trim_end_matches('/'),
        path.trim_start_matches('/')
    );
    let resp = state.http.get(url).send().await.map_err(|e| {
        tracing::error!(error = %e, "sync service unreachable");
        ApiError::Internal
    })?;
    if !resp.status().is_success() {
        if resp.status().as_u16() == 404 {
            return Err(ApiError::NotFound);
        }
        return Err(ApiError::Internal);
    }
    Ok(Json(resp.json().await.map_err(|_| ApiError::Internal)?))
}

/// POST a JSON body to the sync service, tolerating an empty (`204`) response and mapping
/// a "not linked" conflict through to the caller. `pub(crate)` so `admin.rs` can reuse it for
/// operator-triggered force pull/push (design: admin Sync console tab).
pub(crate) async fn sync_proxy(
    state: &AppState,
    path: &str,
    body: serde_json::Value,
) -> ApiResult<Json<serde_json::Value>> {
    let url = format!(
        "{}/{}",
        state.sync_url.trim_end_matches('/'),
        path.trim_start_matches('/')
    );
    let resp = state.http.post(url).json(&body).send().await.map_err(|e| {
        tracing::error!(error = %e, "sync service unreachable");
        ApiError::Internal
    })?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        if status.as_u16() == 409 {
            return Err(ApiError::Conflict("Account not linked".to_owned()));
        }
        if status.as_u16() == 404 {
            return Err(ApiError::NotFound);
        }
        tracing::warn!(%status, body = %text, "sync service returned an error");
        return Err(ApiError::Internal);
    }
    let value = if text.trim().is_empty() {
        serde_json::json!({ "ok": true })
    } else {
        serde_json::from_str(&text).unwrap_or_else(|_| serde_json::json!({ "ok": true }))
    };
    Ok(Json(value))
}
