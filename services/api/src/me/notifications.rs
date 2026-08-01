//! In-app notifications, the activity feed, and the live SSE stream.

use crate::error::{ApiError, ApiResult};
use crate::openapi::{ME_DASHBOARD_TAG, ME_NOTIFICATIONS_TAG};
use crate::state::{AppState, AuthUser};
use axum::Json;
use axum::extract::{Query, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use secrecy::SecretString;
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
    /// Single-use ticket from `POST /v1/me/stream-ticket`, passed as a query parameter because
    /// the browser `EventSource` API cannot attach an `Authorization` header (design §17.4).
    ///
    /// Was the raw access token until SEC-8. A query string is recorded by `TraceLayer` as a
    /// span field, preserved verbatim by the frontend proxy, written to every reverse-proxy
    /// access log and kept in browser history — so the credential that rides here must be worth
    /// nothing by the time anyone reads it back. This one is spent by the request that carries
    /// it, expires in 30 seconds, and opens nothing but this stream.
    pub ticket: String,
}

/// A freshly minted stream ticket.
#[derive(Debug, Serialize, ToSchema)]
pub struct StreamTicket {
    /// The opaque value to pass as `?ticket=` when opening the stream.
    // Handing it to the client is the endpoint's purpose, so it opts into serialisation
    // explicitly — see `crate::secret` for why that opt-in is per field rather than blanket.
    #[serde(serialize_with = "crate::secret::expose_onto_wire")]
    #[schema(value_type = String)]
    pub ticket: SecretString,
    /// Seconds until it expires. Redeem immediately; this is for diagnostics, not scheduling.
    pub expires_in: u64,
}

/// Mint a stream ticket
///
/// Exchanges the caller's `Bearer` session for a single-use, 30-second credential that
/// `EventSource` can carry in a query string. Mint one per connection attempt — including each
/// reconnect, since redeeming a ticket spends it.
#[utoipa::path(
    post,
    path = "/v1/me/stream-ticket",
    tag = ME_NOTIFICATIONS_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "A single-use ticket for `GET /v1/me/stream`", body = StreamTicket),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 503, description = "the ticket store is temporarily unavailable", body = crate::error::ProblemDetails),
    )
)]
pub async fn stream_ticket(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<StreamTicket>> {
    let ticket = state.stream_tickets.mint(user.user_id).await.map_err(|e| {
        tracing::warn!(error = %e, "failed to mint a stream ticket");
        ApiError::Unavailable
    })?;
    Ok(Json(StreamTicket {
        ticket,
        expires_in: crate::stream_tickets::TICKET_TTL.as_secs(),
    }))
}

/// Live notification stream
///
/// Server-Sent Events of live per-user notifications (design §14, §17.4). Authenticated by a
/// single-use `ticket` query parameter obtained from `POST /v1/me/stream-ticket` — not the
/// `Authorization` header, which `EventSource` cannot set. It subscribes to the ticket's own
/// user's core-NATS subject and relays each `UserNotification` as a `notification` SSE event,
/// with a periodic keep-alive comment so proxies keep the connection open. Ownership is
/// implicit: the subscription is scoped to the user the ticket was minted for.
///
/// The ticket is consumed here, so `EventSource`'s *automatic* reconnect cannot re-open this
/// stream — the client has to mint a new ticket per attempt (`web/frontend/src/live.rs` does).
/// That is a feature rather than a cost: re-minting goes through `AuthUser`, so a suspension
/// applied mid-stream is caught by the mint call as well as by the check below.
#[utoipa::path(
    get,
    path = "/v1/me/stream",
    tag = ME_NOTIFICATIONS_TAG,
    params(StreamQuery),
    security(("stream_ticket" = [])),
    responses(
        (status = 200, description = "SSE stream of `notification` events", content_type = "text/event-stream"),
        (status = 401, description = "missing, expired, or already-redeemed ticket", body = crate::error::ProblemDetails),
        (status = 503, description = "the live notification stream is temporarily unavailable", body = crate::error::ProblemDetails),
    )
)]
pub async fn stream(
    State(state): State<AppState>,
    Query(q): Query<StreamQuery>,
) -> ApiResult<Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>>> {
    let user_id = state
        .stream_tickets
        .consume(&q.ticket)
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "failed to redeem a stream ticket");
            ApiError::Unavailable
        })?
        .ok_or(ApiError::Unauthorized)?;

    // The same check the `AuthUser` extractor makes for every other route, which this one
    // skipped because it does not use the extractor. A redeemed ticket proves the holder had a
    // session 30 seconds ago; it does not prove the account still exists or is still permitted
    // to act. Without this, a suspended or deleted user kept receiving their feed for the
    // token's remaining lifetime — the one route where "revoke now" did not mean now.
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

    // Cap the stream so the checks above re-run. Without this they happen only at connect time
    // and one long-lived connection keeps delivering forever, which is the half of SEC-8 that
    // made a suspension take up to 15 minutes to bite. The bound is the access-token lifetime —
    // the same cadence it was when the token's own `exp` capped the stream, deliberately, so
    // replacing the credential with a ticket did not quietly extend the window. When the stream
    // ends the client re-mints and reconnects, and the mint call is itself an `AuthUser` check.
    let deadline = tokio::time::sleep(state.access_ttl.unsigned_abs());
    let events = futures::StreamExt::take_until(events, deadline);

    Ok(Sse::new(events).keep_alive(KeepAlive::default()))
}
