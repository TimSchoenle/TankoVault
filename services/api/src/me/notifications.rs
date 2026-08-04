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

/// Largest page size, matching `/v1/me/watchlist`'s ceiling.
const MAX_LIMIT: i64 = 200;

/// Highest accepted offset, for the same reason `/v1/me/watchlist` clamps its own.
const MAX_OFFSET: i64 = 100_000;

fn default_limit() -> i64 {
    50
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct NotificationsParams {
    /// Page size, clamped to `1..=200`.
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct NotificationsView {
    /// One page of the inbox, newest first. Free-form per notification kind: the notifier writes
    /// an open `payload`, and pinning a schema here would drop every kind it has not shipped yet.
    #[schema(value_type = Vec<serde_json::Value>)]
    pub items: Vec<serde_json::Value>,
    /// Notifications the caller has, in total — the pager's denominator.
    pub total: i64,
    /// Of those, how many are unread. Counted server-side on purpose: derived from `items` it is
    /// only ever the unread count *of the loaded page*, which is what pinned the bell at 100.
    pub unread: i64,
}

/// List notifications
///
/// A page of the caller's inbox, newest first, with the inbox-wide `total` and `unread`.
///
/// The body is an object rather than the bare array it used to be, for the same reason
/// `/v1/me/watchlist` is: neither count is derivable from a page of items, and the frontend —
/// the only consumer, and regenerated from this document — needs both to page and to keep the
/// bell honest past the first page.
#[utoipa::path(
    get,
    path = "/v1/me/notifications",
    tag = ME_NOTIFICATIONS_TAG,
    params(NotificationsParams),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "A page of the caller's notifications", body = NotificationsView),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn notifications(
    State(state): State<AppState>,
    user: AuthUser,
    Query(params): Query<NotificationsParams>,
) -> ApiResult<Json<NotificationsView>> {
    let page = tankovault_db::repo::tracking::notifications_page(
        &state.pool,
        user.user_id,
        params.limit.clamp(1, MAX_LIMIT),
        params.offset.clamp(0, MAX_OFFSET),
    )
    .await?;
    Ok(Json(NotificationsView {
        items: page
            .items
            .into_iter()
            .map(|n| serde_json::to_value(n).unwrap_or_default())
            .collect(),
        total: page.total,
        unread: page.unread,
    }))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct MarkRead {
    /// The notifications to mark read. Ignored when `all` is set.
    #[serde(default)]
    pub ids: Vec<Uuid>,
    /// Mark the caller's whole inbox read, rather than the listed ids.
    ///
    /// A client only ever holds the page it loaded, so "mark all read" sent as a list of ids
    /// marks one page and quietly leaves the rest unread.
    #[serde(default)]
    pub all: bool,
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
    let n = if body.all {
        tankovault_db::repo::tracking::notifications_mark_all_read(&state.pool, user.user_id)
            .await?
    } else {
        tankovault_db::repo::tracking::notifications_mark_read(&state.pool, user.user_id, &body.ids)
            .await?
    };
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
    // Opts into serialisation explicitly, per crate::secret's per-field convention.
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

/// Holds one unit of `sse_streams_active` for the lifetime of a stream.
///
/// The gauge exists because these connections are close to invisible in the request metrics:
/// `http_requests_total` and the latency histogram are recorded from a response, and an SSE
/// response ends by the client disconnecting, which drops the middleware future before there
/// is one. This is the only honest count of connected browsers.
struct StreamGuard;

impl StreamGuard {
    fn enter() -> Self {
        metrics::gauge!("sse_streams_active").increment(1.0);
        Self
    }
}

impl Drop for StreamGuard {
    fn drop(&mut self) {
        metrics::gauge!("sse_streams_active").decrement(1.0);
    }
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

    // Same check `AuthUser` makes elsewhere, done manually since this route skips it: a
    // redeemed ticket only proves a session existed 30s ago, not that the account may still act.
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

    // Held by the stream, so the decrement runs when the response future is dropped — which
    // for SSE is the *normal* ending, the browser closing the tab. Same reason
    // `http_requests_in_flight` uses a guard rather than a trailing statement.
    let open = StreamGuard::enter();
    let events = subscriber.map(move |msg| {
        // Bound into the closure so the guard lives exactly as long as the stream does.
        let _open = &open;
        let (event, result) = match serde_json::from_slice::<UserNotification>(&msg.payload) {
            Ok(notification) => match Event::default()
                .event("notification")
                .json_data(&notification)
            {
                Ok(event) => (event, "ok"),
                Err(_) => (Event::default().comment("serialize error"), "error"),
            },
            Err(_) => (
                Event::default().comment("undecodable notification"),
                "undecodable",
            ),
        };
        metrics::counter!("sse_events_pushed_total", "result" => result).increment(1);
        Ok(event)
    });

    // Capped to the access-token lifetime so these checks re-run periodically, not just at
    // connect — a ticket must not quietly extend the window the token used to bound.
    let deadline = tokio::time::sleep(state.access_ttl.unsigned_abs());
    let events = futures::StreamExt::take_until(events, deadline);

    Ok(Sse::new(events).keep_alive(KeepAlive::default()))
}
