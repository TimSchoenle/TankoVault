//! Live per-user notification stream (design §14, §17.4).
//!
//! The browser's `EventSource` connects to the API's `/v1/me/stream` SSE endpoint (the token
//! rides in the query string because `EventSource` cannot set headers) and each push updates
//! the rail's unread badge in real time. This is a best-effort enhancement layered on top of
//! the durable notifications list: if the stream drops, the badge simply stops updating until
//! the next navigation refetch, and no data is lost.

use crate::api;
use crate::components::UnreadBadge;
use crate::models::LiveNotification;
use dioxus::prelude::*;
use futures_util::StreamExt;
use gloo_net::eventsource::futures::EventSource;

/// Subscribe to the live-notification stream for `token`, updating `badge` as pushes arrive.
///
/// Runs until the returned future is dropped — which the caller's `use_resource` does
/// automatically when the access token changes or the user signs out, dropping the
/// `EventSource` and closing the connection.
pub async fn run(token: String, badge: UnreadBadge) {
    let mut source = match EventSource::new(&api::stream_url(&token)) {
        Ok(source) => source,
        // A malformed URL is the only failure here; nothing actionable, so degrade quietly.
        Err(_) => return,
    };
    let mut subscription = match source.subscribe("notification") {
        Ok(subscription) => subscription,
        Err(_) => return,
    };

    let mut badge_signal = badge.0;
    while let Some(item) = subscription.next().await {
        // A transient connection error surfaces as an `Err` item; the browser reconnects on
        // its own, so keep awaiting rather than tearing the loop down.
        if let Ok((_event_type, message)) = item {
            if let Some(text) = message.data().as_string() {
                if let Ok(push) = serde_json::from_str::<LiveNotification>(&text) {
                    badge_signal.set(push.unread_count);
                }
            }
        }
    }

    source.close();
}
