//! In-app notifications, the activity feed, and the live SSE stream.

use crate::error::{ApiError, ApiResult};
use crate::openapi::{ME_DASHBOARD_TAG, ME_NOTIFICATIONS_TAG};
use crate::state::{AppState, AuthUser};
use axum::Json;
use axum::extract::{Query, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use tankovault_contracts::UserNotification;
use tankovault_domain::{SeriesId, resolve_link};
use time::OffsetDateTime;
use tokio_stream::StreamExt as _;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

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

    // The same check the `AuthUser` extractor makes for every other route, which this one
    // skipped because it does not use the extractor. A valid signature proves the token was
    // ours; it does not prove the account still exists or is still permitted to act. Without
    // this, a suspended or deleted user kept receiving their feed for the token's remaining
    // lifetime — the one route where "revoke now" did not mean now.
    let principal = tankovault_db::repo::permissions::resolve(&state.pool, user_id)
        .await?
        .ok_or(ApiError::Unauthorized)?;
    if !principal.status.may_authenticate() {
        return Err(ApiError::Suspended);
    }

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

    // The stream outlives the token that opened it, so cap it at that token's own expiry.
    // Without this the suspension check above runs only at connect time, and one long-lived
    // `EventSource` keeps delivering forever. `EventSource` reconnects on its own when the
    // stream ends, and the reconnect is re-checked — which is exactly the behaviour wanted.
    let remaining = (claims.exp - OffsetDateTime::now_utc().unix_timestamp()).max(0);
    let deadline = tokio::time::sleep(std::time::Duration::from_secs(remaining.unsigned_abs()));
    let events = futures::StreamExt::take_until(events, deadline);

    Ok(Sse::new(events).keep_alive(KeepAlive::default()))
}
