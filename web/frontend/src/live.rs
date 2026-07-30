//! Live per-user notification stream (design §14, §17.4).
//!
//! The browser's `EventSource` connects to the API's `/v1/me/stream` SSE endpoint — the credential
//! rides in the query string because `EventSource` cannot set headers — and each push updates the
//! rail's unread badge in real time.
//!
//! This is a best-effort enhancement over the durable notifications list: if the stream drops, the
//! badge simply stops updating until the next navigation refetch. No data is lost, so a failure
//! here is deliberately silent rather than an error state in the reader's face.
//!
//! # Why this reconnects itself
//!
//! The query credential is a **single-use, 30-second ticket** minted by `POST
//! /v1/me/stream-ticket`, not the access token (SEC-8). Opening the stream spends it, so
//! `EventSource`'s own automatic reconnect — which replays the same URL — can no longer work: the
//! retry would present a spent ticket and draw a `401`, and per spec `EventSource` gives up for
//! good on a non-200. [`run`] therefore drives the reconnect itself, minting a fresh ticket each
//! time, with backoff so a persistent failure does not spin.
//!
//! That is a security property as much as a mechanical one. The API caps each stream at one
//! access-token lifetime precisely so its suspension check re-runs; re-minting is what makes the
//! reconnect happen, and the mint call goes through the `AuthUser` extractor, so a suspension
//! applied mid-stream is refused by the mint *and* by the stream.

use crate::api::Api;
use crate::components::UnreadBadge;
use crate::models::LiveNotification;
use dioxus::prelude::*;
use futures_util::StreamExt;
use gloo_net::eventsource::futures::EventSource;
use gloo_timers::future::TimeoutFuture;

/// First wait after a failed attempt; doubles up to [`RECONNECT_BACKOFF_MAX_MS`].
const RECONNECT_BACKOFF_START_MS: u32 = 1_000;
/// Ceiling on the reconnect wait. The stream is a nicety, so a slow retry is the right failure.
const RECONNECT_BACKOFF_MAX_MS: u32 = 60_000;
/// How long an attempt has to stay open before it counts as having worked.
///
/// The API ends each stream on purpose at the access token's lifetime so its account checks re-run,
/// and at this layer that is indistinguishable from a failure: `EventSource` retries the closed
/// connection itself, the retry presents the spent ticket, and the `401` surfaces as the same
/// `Err` a rejected ticket would. Duration is the signal that separates them — a refused ticket
/// fails in milliseconds, a served stream lasts minutes — and it is what keeps a healthy
/// long-lived stream from ratcheting the backoff up every time it is deliberately recycled.
const SETTLED_MS: f64 = 5_000.0;

/// Subscribe to the live-notification stream, updating `badge` as pushes arrive.
///
/// Runs until the returned future is dropped — which the caller's `use_resource` does
/// automatically when the session token changes or the user signs out, dropping the `EventSource`
/// and closing the connection.
pub(crate) async fn run(api: Api, badge: UnreadBadge) {
    let mut backoff_ms = RECONNECT_BACKOFF_START_MS;
    loop {
        // A fresh ticket per attempt: redeeming one spends it.
        let Ok(response) = api.client().stream_ticket().send().await else {
            // Includes the `401` of a session that has gone away, and the `403` of a suspension.
            // Backing off rather than giving up keeps a recovered session picked up without a
            // reload, and the caller's `use_resource` tears this whole future down on sign-out.
            TimeoutFuture::new(backoff_ms).await;
            backoff_ms = backoff_ms.saturating_mul(2).min(RECONNECT_BACKOFF_MAX_MS);
            continue;
        };
        let ticket = response.into_inner().ticket;

        // The attempt that served a real stream resets the wait; a run of failures backs off.
        if consume(&api, &ticket, badge).await {
            backoff_ms = RECONNECT_BACKOFF_START_MS;
        }
        TimeoutFuture::new(backoff_ms).await;
        backoff_ms = backoff_ms.saturating_mul(2).min(RECONNECT_BACKOFF_MAX_MS);
    }
}

/// Open one stream with `ticket` and pump it until it ends.
///
/// Returns whether the attempt is judged to have *worked* — see [`SETTLED_MS`] for why that is a
/// duration rather than a status.
async fn consume(api: &Api, ticket: &str, badge: UnreadBadge) -> bool {
    let url = format!("{}{}", api.base_url(), crate::api::stream_url(ticket));
    let Ok(mut source) = EventSource::new(&url) else {
        // A malformed URL is the only failure mode here; nothing is actionable.
        return false;
    };
    let Ok(mut subscription) = source.subscribe("notification") else {
        source.close();
        return false;
    };

    let started = js_sys::Date::now();
    let mut badge = badge.0;
    while let Some(item) = subscription.next().await {
        // `EventSource` retries a dropped connection on its own before surfacing an `Err`, and with
        // a spent ticket that retry cannot succeed — so an error here ends this attempt and the
        // caller mints a new ticket, rather than being ignored as it was when the credential in the
        // URL was reusable and the browser could recover unaided.
        let Ok((_event, message)) = item else {
            break;
        };
        let Some(text) = message.data().as_string() else {
            continue;
        };
        if let Ok(push) = serde_json::from_str::<LiveNotification>(&text) {
            badge.set(push.unread_count);
        }
    }

    source.close();
    js_sys::Date::now() - started >= SETTLED_MS
}
