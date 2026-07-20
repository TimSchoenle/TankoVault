//! Authenticated user tracking: watchlist, read progress, notifications. Ownership is
//! enforced implicitly — every query is scoped to the token's `user_id`.

use crate::error::{ApiError, ApiResult};
use crate::state::{AppState, AuthUser};
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use tankovault_contracts::UserNotification;
use tankovault_domain::{SeriesId, WatchStatus, resolve_link};
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
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
}

/// `GET /v1/me/watchlist`
pub async fn watchlist(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<Vec<WatchlistItem>>> {
    let entries = tankovault_db::repo::tracking::watchlist_list(&state.pool, user.user_id).await?;
    let out = entries
        .into_iter()
        .map(|e| WatchlistItem {
            series_id: e.series_id,
            status: e.status,
            notify: e.notify,
            added_at: e.added_at,
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
    pub last_read_number: f64,
}

/// `PUT /v1/me/progress/:series_id`
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
        body.last_read_number,
    )
    .await?;
    Ok(Json(serde_json::json!({ "ok": true })))
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

/// `GET /v1/me/sync/anilist/authorize` — returns the `AniList` consent URL (proxied).
pub async fn sync_authorize_url(
    State(state): State<AppState>,
    _user: AuthUser,
) -> ApiResult<Json<serde_json::Value>> {
    let url = format!(
        "{}/v1/anilist/authorize-url",
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

#[derive(Debug, Deserialize)]
pub struct AniListCallback {
    pub code: String,
}

/// `GET /v1/me/sync/anilist/callback?code=…` — exchanges the code and links the account.
pub async fn sync_callback(
    State(state): State<AppState>,
    user: AuthUser,
    Query(q): Query<AniListCallback>,
) -> ApiResult<Json<serde_json::Value>> {
    sync_proxy(
        &state,
        "/v1/anilist/link",
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

/// `POST /v1/me/sync/anilist/push` — reflect local watchlist/progress to `AniList`.
pub async fn sync_push(
    State(state): State<AppState>,
    user: AuthUser,
    body: Option<Json<SyncOpts>>,
) -> ApiResult<Json<serde_json::Value>> {
    let opts = body.map(|b| b.0).unwrap_or_default();
    sync_proxy(
        &state,
        "/v1/anilist/push",
        serde_json::json!({ "user_id": user.user_id, "policy": opts.policy }),
    )
    .await
}

/// `POST /v1/me/sync/anilist/pull` — import the `AniList` list into the local watchlist.
pub async fn sync_pull(
    State(state): State<AppState>,
    user: AuthUser,
    body: Option<Json<SyncOpts>>,
) -> ApiResult<Json<serde_json::Value>> {
    let opts = body.map(|b| b.0).unwrap_or_default();
    sync_proxy(
        &state,
        "/v1/anilist/pull",
        serde_json::json!({ "user_id": user.user_id, "policy": opts.policy }),
    )
    .await
}

/// POST a JSON body to the sync service, tolerating an empty (`204`) response and mapping
/// a "not linked" conflict through to the caller.
async fn sync_proxy(
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
            return Err(ApiError::Conflict("AniList account not linked".to_owned()));
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
