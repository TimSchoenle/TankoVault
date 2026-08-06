//! The operator console's single live stream.
//!
//! One SSE connection carries every push the console needs, on the cadence each payload
//! deserves, instead of a dozen panels re-issuing their own GET against a shared four-second
//! timer. `/v1/admin/scans/stream` predates this and stays for one release; it is unreachable
//! from a browser (see below) and is why the console polled at all.

use crate::error::{ApiError, ApiResult};
use crate::me::notifications::{StreamGuard, StreamQuery};
use crate::openapi::ADMIN_OVERVIEW_TAG;
use crate::state::AppState;
use crate::views::IntoView as _;
use axum::extract::{Query, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use futures::stream::{BoxStream, StreamExt as _};
use std::convert::Infallible;
use std::time::Duration;
use tankovault_db::repo::permissions::Principal;
use tankovault_domain::{Feature, Permission};
use tokio_stream::wrappers::IntervalStream;

/// How often a run's progress is re-read. A scan in flight changes on this timescale; anything
/// slower and the queue an operator is watching lies to them.
const RUNS_PERIOD: Duration = Duration::from_secs(2);
/// How often the system counters are re-read. They move slowly and the aggregate behind them is
/// the heaviest read on the admin surface — the old whole-console four-second poll was both too
/// fast for these and too slow for a run.
const STATS_PERIOD: Duration = Duration::from_secs(10);
/// How many runs the `runs` event carries. Matches what the panel renders.
const RUNS_LIMIT: i64 = 20;

/// Console live stream
///
/// Server-Sent Events for the operator console: `stats` every 10 s and `runs` every 2 s.
///
/// Authenticated by a single-use `ticket` query parameter from `POST /v1/me/stream-ticket`,
/// because `EventSource` cannot set an `Authorization` header. A ticket proves a session
/// existed thirty seconds ago and nothing more, so the permission check below happens *after*
/// redemption and gates each event separately: a caller entitled to scan runs but not to system
/// counters receives `runs` and never `stats`. Emitting an event the caller could not fetch
/// over its own GET would be a disclosure, and the access-matrix suites would not catch it —
/// they reconcile status codes, not event names.
#[utoipa::path(
    get,
    path = "/v1/admin/stream",
    tag = ADMIN_OVERVIEW_TAG,
    params(StreamQuery),
    // The literal is what `utoipa` accepts here; `crate::openapi::STREAM_TICKET_AUTH` is the
    // same name, registered as an `apiKey` in `query`.
    security(("stream_ticket" = [])),
    responses(
        (status = 200, description = "SSE stream of `stats` and `runs` events", content_type = "text/event-stream"),
        (status = 401, description = "missing, expired, or already-redeemed ticket", body = crate::error::ProblemDetails),
        (status = 403, description = "the caller holds none of the permissions this stream carries", body = crate::error::ProblemDetails),
        (status = 503, description = "the ticket store is temporarily unavailable", body = crate::error::ProblemDetails),
    )
)]
pub async fn admin_stream(
    State(state): State<AppState>,
    Query(q): Query<StreamQuery>,
) -> ApiResult<Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>>> {
    let principal = redeem(&state, &q.ticket).await?;

    // Both halves of what the console's rail already checks: the permission *and* the feature.
    // A deployment with `admin.stats` switched off has no counters to push.
    let may_stats = principal.permissions.has(Permission::SystemStats)
        && state.features.is_enabled(Feature::AdminStats);
    let may_runs = principal.permissions.has(Permission::ScansRead)
        && state.features.is_enabled(Feature::ScanningManual);
    if !may_stats && !may_runs {
        return Err(ApiError::Forbidden);
    }

    let mut sources: Vec<BoxStream<'static, Result<Event, Infallible>>> = Vec::new();
    if may_runs {
        let pool = state.pool.clone();
        sources.push(
            IntervalStream::new(tokio::time::interval(RUNS_PERIOD))
                .then(move |_| {
                    let pool = pool.clone();
                    async move {
                        let runs =
                            tankovault_db::repo::scans::list_recent_runs(&pool, RUNS_LIMIT).await;
                        Ok(named("runs", runs.unwrap_or_default()))
                    }
                })
                .boxed(),
        );
    }
    if may_stats {
        let stats_state = state.clone();
        sources.push(
            IntervalStream::new(tokio::time::interval(STATS_PERIOD))
                .then(move |_| {
                    let stats_state = stats_state.clone();
                    async move {
                        // Through the same snapshot cache `GET /v1/admin/stats` uses, never
                        // straight at the aggregate: every column is a `count(*)` over a whole
                        // table, and one query per connected operator per tick is the outage
                        // this endpoint exists to avoid.
                        let pool = stats_state.pool.clone();
                        let snapshot =
                            stats_state
                                .system_stats
                                .get(move || {
                                    let pool = pool.clone();
                                    async move {
                                        tankovault_db::repo::stats::system_overview(&pool).await
                                    }
                                })
                                .await;
                        match snapshot {
                            Ok(overview) => Ok(named("stats", overview.into_view())),
                            // A failed aggregate skips this tick rather than ending the stream:
                            // the console would reconnect and re-run the same failing query.
                            Err(_) => Ok(Event::default().comment("stats unavailable")),
                        }
                    }
                })
                .boxed(),
        );
    }

    let open = StreamGuard::enter();
    let events = futures::stream::select_all(sources).map(move |event| {
        // Bound into the closure so the gauge decrements when the stream is dropped — which for
        // SSE is the normal ending, the operator closing the tab.
        let _open = &open;
        metrics::counter!("sse_events_pushed_total", "result" => "ok").increment(1);
        event
    });

    // Capped to the access-token lifetime, exactly as `/v1/me/stream` is: a ticket must not
    // quietly extend the window the token bounded, and re-minting re-runs the checks above.
    let deadline = tokio::time::sleep(state.access_ttl.unsigned_abs());
    let events = futures::StreamExt::take_until(events, deadline);
    Ok(Sse::new(events).keep_alive(KeepAlive::default()))
}

/// Spend `ticket` and resolve the principal behind it.
///
/// The status re-check is not redundant with minting: a ticket is valid for thirty seconds, and
/// an account suspended inside that window must not open a stream on the strength of it.
async fn redeem(state: &AppState, ticket: &str) -> ApiResult<Principal> {
    let user_id = state
        .stream_tickets
        .consume(ticket)
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "failed to redeem a stream ticket");
            ApiError::Unavailable
        })?
        .ok_or(ApiError::Unauthorized)?;

    let principal = tankovault_db::repo::permissions::resolve(&state.pool, user_id)
        .await?
        .ok_or(ApiError::Unauthorized)?;
    if !principal.status.may_authenticate() {
        return Err(ApiError::Suspended);
    }
    Ok(principal)
}

/// One named SSE event, or a comment if the payload will not serialise.
fn named<T: serde::Serialize>(name: &str, payload: T) -> Event {
    Event::default()
        .event(name)
        .json_data(&payload)
        .unwrap_or_else(|_| Event::default().comment("serialize error"))
}
