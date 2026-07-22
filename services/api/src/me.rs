//! Authenticated user tracking: watchlist, read progress, notifications. Ownership is
//! enforced implicitly — every query is scoped to the token's `user_id`.

use crate::error::{ApiError, ApiResult};
use crate::state::{AppState, AuthUser};
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use tankovault_contracts::UserNotification;
use tankovault_domain::{SeriesId, UserId, WatchStatus, resolve_link};
use time::OffsetDateTime;
use tokio_stream::StreamExt as _;
use uuid::Uuid;

#[derive(Debug, Serialize)]
pub struct WatchlistItem {
    pub series_id: SeriesId,
    pub status: WatchStatus,
    pub notify: bool,
    #[serde(with = "time::serde::rfc3339")]
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

/// `GET /v1/me/watchlist` — the user's watchlist with embedded title/cover/progress
/// (frontend §9.3). Extra fields are additive; older clients ignore them.
pub async fn watchlist(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<Vec<WatchlistItem>>> {
    let cards = tankovault_db::repo::tracking::watchlist_detailed(&state.pool, user.user_id).await?;
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

#[derive(Debug, Deserialize)]
pub struct WatchlistUpsert {
    #[serde(default)]
    pub status: WatchStatus,
    #[serde(default = "default_true")]
    pub notify: bool,
}

fn default_true() -> bool {
    true
}

/// `PUT /v1/me/watchlist/:series_id`
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

/// `DELETE /v1/me/watchlist/:series_id`
pub async fn delete_watchlist(
    State(state): State<AppState>,
    user: AuthUser,
    Path(series_id): Path<SeriesId>,
) -> ApiResult<Json<serde_json::Value>> {
    tankovault_db::repo::tracking::watchlist_remove(&state.pool, user.user_id, series_id).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Debug, Deserialize)]
pub struct ProgressUpdate {
    /// The whole-chapter frontier to set outright (design v2 §A.6 — renamed from
    /// `last_read_number`, same semantics).
    pub last_read_whole_number: f64,
}

/// `PUT /v1/me/progress/:series_id` — set the whole-chapter frontier outright (design v2 §A.6).
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

#[derive(Debug, Serialize)]
pub struct ProgressDto {
    pub last_read_whole_number: f64,
    pub last_read_part_number: Option<f64>,
}

/// `GET /v1/me/progress/:series_id` — both read frontiers for the series (design v2 §A.6).
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

#[derive(Debug, Deserialize)]
pub struct ChapterRead {
    pub read: bool,
}

/// `PUT /v1/me/progress/:series_id/chapters/:number` — apply the §A.3 mark-read/mark-unread
/// rule for one chapter number. Unmarking an older (non-frontier) chapter retreats progress
/// past it too; the client must confirm with the user first in that case (design v2 §A.6).
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

#[derive(Debug, Deserialize)]
pub struct MarkReadTo {
    pub number: f64,
}

/// `POST /v1/me/progress/:series_id/mark-read-to` — "mark read to here" (design v2 §A.6);
/// equivalent to marking `number` read.
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

#[derive(Debug, Deserialize)]
pub struct SyncExcluded {
    pub excluded: bool,
}

/// `PUT /v1/me/watchlist/:series_id/sync` — set the blanket per-series sync-exclusion flag
/// (design v2 §A.5).
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

/// `PUT /v1/me/watchlist/:series_id/sync/:provider` — per-provider override of the blanket
/// exclusion flag (design v2 §A.5).
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

/// `GET /v1/me/notifications`
pub async fn notifications(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<serde_json::Value>> {
    let list =
        tankovault_db::repo::tracking::notifications_list(&state.pool, user.user_id, 100).await?;
    Ok(Json(serde_json::to_value(list).unwrap_or_default()))
}

#[derive(Debug, Deserialize)]
pub struct MarkRead {
    pub ids: Vec<Uuid>,
}

/// `POST /v1/me/notifications/read`
pub async fn mark_read(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<MarkRead>,
) -> ApiResult<Json<serde_json::Value>> {
    let n =
        tankovault_db::repo::tracking::notifications_mark_read(&state.pool, user.user_id, &body.ids)
            .await?;
    Ok(Json(serde_json::json!({ "marked": n })))
}

#[derive(Debug, Serialize)]
pub struct FeedEntry {
    pub series_id: SeriesId,
    pub series_title: String,
    pub chapter_number: f64,
    pub chapter_title: Option<String>,
    pub provider_slug: String,
    /// Ready-to-open absolute URL, resolved from the provider base + relative path.
    pub url: String,
    #[serde(with = "time::serde::rfc3339")]
    pub discovered_at: OffsetDateTime,
}

/// `GET /v1/me/feed` — unread chapters across the watchlist (the reading dashboard).
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

#[derive(Debug, Serialize)]
pub struct ContinueItem {
    pub series_id: SeriesId,
    pub series_title: String,
    pub cover_url: Option<String>,
    pub last_read_number: f64,
    /// The lowest unread chapter number above the user's progress, if any.
    pub next_number: Option<f64>,
    pub unread: i64,
}

/// `GET /v1/me/continue` — continue-reading cards for Home / the Series CTA (frontend §9.3):
/// tracked, in-progress series that have unread chapters, freshest activity first.
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

/// `GET /v1/me/recommendations` — "Because you read" suggestions (frontend §9.3, *Stub*):
/// unwatched series sharing tags with the user's list. Falls back to the most-recent catalog
/// when the user has no tagged watchlist yet, so the shelf is never empty for signed-in users.
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

/// `GET /v1/me/stats` — lifetime tracking stats for the Home / Profile headline
/// (frontend §9.3, *Stub*). See `tankovault_db::repo::tracking::MeStats` for the honest
/// definition of `chapters_read` and why no "streak" is returned.
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

#[derive(Debug, Deserialize)]
pub struct ProfileUpdate {
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ProfileDto {
    pub id: uuid::Uuid,
    pub email: String,
    pub username: String,
    pub role: String,
}

/// `PATCH /v1/me/profile` — update the caller's username and/or email (frontend §9.4).
/// A duplicate email/username surfaces as `409 Conflict`.
pub async fn patch_profile(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<ProfileUpdate>,
) -> ApiResult<Json<ProfileDto>> {
    let username = body.username.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let email = body.email.as_deref().map(str::trim).filter(|s| !s.is_empty());
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

#[derive(Debug, Serialize)]
pub struct SessionDto {
    pub id: String,
    pub family_id: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
}

/// `GET /v1/me/sessions` — the caller's active login sessions (frontend §9.4).
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

/// `DELETE /v1/me/sessions/:id` — revoke one of the caller's own sessions (frontend §9.4).
/// Scoped to ownership; a foreign/unknown id yields `404`.
pub async fn delete_session(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let revoked =
        tankovault_db::repo::users::revoke_session(&state.pool, user.user_id, id).await?;
    if revoked == 0 {
        return Err(ApiError::NotFound);
    }
    Ok(Json(serde_json::json!({ "revoked": revoked })))
}

/// `GET /v1/me/notification-prefs` — the caller's notification preferences JSON (frontend
/// §9.4). `{}` means "product defaults".
pub async fn notification_prefs(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<serde_json::Value>> {
    Ok(Json(
        tankovault_db::repo::users::get_notification_prefs(&state.pool, user.user_id).await?,
    ))
}

/// `PUT /v1/me/notification-prefs` — replace the caller's notification preferences (frontend
/// §9.4). The body is stored verbatim as an open JSON document.
pub async fn put_notification_prefs(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<serde_json::Value>,
) -> ApiResult<Json<serde_json::Value>> {
    tankovault_db::repo::users::set_notification_prefs(&state.pool, user.user_id, &body).await?;
    Ok(Json(body))
}

#[derive(Debug, Deserialize)]
pub struct StreamQuery {
    /// Access token, passed as a query parameter because the browser `EventSource` API
    /// cannot attach an `Authorization` header (design §17.4). It is verified exactly like
    /// a `Bearer` token and never logged.
    pub access_token: String,
}

/// `GET /v1/me/stream` — Server-Sent Events of live per-user notifications (design §14,
/// §17.4). Authenticated by the `access_token` query parameter, it subscribes to the user's
/// core-NATS subject and relays each [`UserNotification`] as a `notification` SSE event,
/// with a periodic keep-alive comment so proxies keep the connection open. Ownership is
/// implicit: the subscription is scoped to the token's own `user_id`.
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

/// `GET /v1/me/sync/providers` — the registered external providers (design: generalized
/// multi-provider sync). Drives the Account "Sync & integrations" panel, which renders one
/// card per entry instead of a single hardcoded `AniList` block.
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

/// `GET /v1/me/sync/:provider/authorize` — returns the provider's consent URL (proxied).
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

/// `GET /v1/me/sync/:provider/status` — whether the caller has a linked account at `provider`,
/// plus the connected display name and most recent sync time (Sync & integrations panel,
/// header pill, Series tracking card). Always `200`; an unlinked account reads
/// `{ "linked": false }`.
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

/// `DELETE /v1/me/sync/:provider` — unlink the caller's account at `provider`.
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

#[derive(Debug, Deserialize)]
pub struct AniListCallback {
    pub code: String,
}

/// `GET /v1/me/sync/:provider/callback?code=…` — exchanges the code and links the account.
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

#[derive(Debug, Deserialize, Default)]
pub struct SyncOpts {
    /// `local_wins` | `remote_wins` | `newest_wins`; omitted uses the service default.
    #[serde(default)]
    pub policy: Option<String>,
}

/// `POST /v1/me/sync/:provider/push` — reflect local watchlist/progress to `provider`
/// (bulk, full-reconciliation walk — see `spawn_targeted_push` for the fast per-series path
/// used automatically when marking a chapter/series read).
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

/// `POST /v1/me/sync/:provider/pull` — import `provider`'s list into the local watchlist.
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

/// `GET /v1/me/sync/:provider/settings` — the caller's automatic-sync settings (design v2 §B.6).
pub async fn sync_settings(
    State(state): State<AppState>,
    user: AuthUser,
    Path(provider): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    sync_get(
        &state,
        &format!(
            "/v1/sync/{provider}/settings/{}",
            user.user_id.as_uuid()
        ),
    )
    .await
}

#[derive(Debug, Deserialize)]
pub struct SyncSettingsPatch {
    #[serde(default)]
    pub auto_sync_enabled: Option<bool>,
    #[serde(default)]
    pub conflict_policy: Option<String>,
}

/// `PATCH /v1/me/sync/:provider/settings` — update automatic sync + conflict policy (design v2
/// §B.6).
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
    let resp = state.http.patch(url).json(&payload).send().await.map_err(|e| {
        tracing::error!(error = %e, "sync service unreachable");
        ApiError::Internal
    })?;
    if !resp.status().is_success() {
        return Err(ApiError::Internal);
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// `GET /v1/me/sync/conflicts` — the caller's pending conflicts across all providers (§B.6).
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

#[derive(Debug, Deserialize)]
pub struct ResolveConflict {
    pub resolution: String,
}

/// `POST /v1/me/sync/conflicts/:id/resolve` — apply the caller's chosen resolution (§B.6).
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

#[derive(Debug, Deserialize)]
pub struct HistoryParams {
    #[serde(default)]
    pub series_id: Option<Uuid>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub page: Option<i64>,
}

/// `GET /v1/me/sync/history` — a page of the caller's sync history (§B.6).
pub async fn sync_history(
    State(state): State<AppState>,
    user: AuthUser,
    Query(q): Query<HistoryParams>,
) -> ApiResult<Json<serde_json::Value>> {
    let mut path = format!("/v1/sync/history/{}?", user.user_id.as_uuid());
    if let Some(s) = q.series_id {
        path.push_str(&format!("series_id={s}&"));
    }
    if let Some(p) = &q.provider {
        path.push_str(&format!("provider={p}&"));
    }
    path.push_str(&format!("page={}", q.page.unwrap_or(0)));
    sync_get(&state, &path).await
}

/// GET a JSON body from the sync service, mapping upstream errors like `sync_proxy`.
pub(crate) async fn sync_get(
    state: &AppState,
    path: &str,
) -> ApiResult<Json<serde_json::Value>> {
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
