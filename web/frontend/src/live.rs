//! Live per-user notification stream (design §14, §17.4).
//!
//! Connects to `/v1/me/stream`, updating the rail's unread badge; a dropped stream fails
//! silently and just waits for the next navigation refetch.
//!
//! The query credential is a single-use ticket rather than the access token, so a transport's
//! own reconnect would replay a spent ticket into a `401` loop — [`run`] reconnects itself
//! instead, minting a fresh ticket each attempt (which also re-triggers the suspension check).

use crate::api::Api;
use crate::components::UnreadBadge;
use crate::models::LiveNotification;
use dioxus::prelude::*;

/// First wait after a failed attempt; doubles up to [`RECONNECT_BACKOFF_MAX_MS`].
const RECONNECT_BACKOFF_START_MS: u32 = 1_000;
/// Ceiling on the reconnect wait. The stream is a nicety, so a slow retry is the right failure.
const RECONNECT_BACKOFF_MAX_MS: u32 = 60_000;
/// How long an attempt has to stay open before it counts as having worked.
///
/// The API deliberately ends each stream at the token's lifetime, which looks identical to a
/// failure here — duration tells them apart: a refused ticket fails in milliseconds, a served
/// stream lasts minutes. Without this, a healthy stream would ratchet the backoff up every
/// time it's recycled.
const SETTLED_MS: f64 = 5_000.0;

/// Subscribe to the live-notification stream, updating `badge` as pushes arrive.
///
/// Runs until dropped — the caller's `use_resource` does that on a token change or sign-out,
/// closing the stream.
pub(crate) async fn run(api: Api, badge: UnreadBadge) {
    let mut backoff_ms = RECONNECT_BACKOFF_START_MS;
    loop {
        // A fresh ticket per attempt: redeeming one spends it.
        let Ok(response) = api.client().stream_ticket().send().await else {
            // Covers a gone-away session (401) and a suspension (403); backing off rather than
            // giving up resumes a recovered session without a reload.
            crate::platform::sleep_ms(backoff_ms).await;
            backoff_ms = backoff_ms.saturating_mul(2).min(RECONNECT_BACKOFF_MAX_MS);
            continue;
        };
        let ticket = response.into_inner().ticket;

        // The attempt that served a real stream resets the wait; a run of failures backs off.
        if consume(&api, &ticket, badge).await {
            backoff_ms = RECONNECT_BACKOFF_START_MS;
        }
        crate::platform::sleep_ms(backoff_ms).await;
        backoff_ms = backoff_ms.saturating_mul(2).min(RECONNECT_BACKOFF_MAX_MS);
    }
}

/// Open one stream with `ticket` and pump it until it ends.
///
/// Returns whether the attempt is judged to have *worked* — see [`SETTLED_MS`] for why that is a
/// duration rather than a status.
async fn consume(api: &Api, ticket: &str, badge: UnreadBadge) -> bool {
    let url = format!("{}{}", api.base_url(), crate::api::stream_url(ticket));
    let Some(mut stream) = crate::platform::subscribe(&url, &["notification"]).await else {
        // A malformed URL or a refused connection; neither is actionable here.
        return false;
    };

    let started = crate::platform::now_ms();
    let mut badge = badge.0;
    while let Some((_name, text)) = stream.next().await {
        if let Ok(push) = serde_json::from_str::<LiveNotification>(&text) {
            badge.set(push.unread_count);
        }
    }

    stream.close();
    crate::platform::now_ms() - started >= SETTLED_MS
}
