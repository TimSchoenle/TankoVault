//! Live per-user notification stream (design §14, §17.4).
//!
//! The browser's `EventSource` connects to the API's `/v1/me/stream` SSE endpoint — the token
//! rides in the query string because `EventSource` cannot set headers — and each push updates
//! the rail's unread badge in real time.
//!
//! This is a best-effort enhancement over the durable notifications list: if the stream drops,
//! the badge simply stops updating until the next navigation refetch. No data is lost, so a
//! failure here is deliberately silent rather than an error state in the reader's face.

use crate::api::Api;
use crate::components::UnreadBadge;
use crate::models::LiveNotification;
use dioxus::prelude::*;
use futures_util::StreamExt;
use gloo_net::eventsource::futures::EventSource;

/// Subscribe to the live-notification stream for `token`, updating `badge` as pushes arrive.
///
/// Runs until the returned future is dropped — which the caller's `use_resource` does
/// automatically when the token changes or the user signs out, dropping the `EventSource`
/// and closing the connection.
pub(crate) async fn run(api: Api, token: String, badge: UnreadBadge) {
    let url = format!("{}{}", api.base_url(), crate::api::stream_url(&token));
    let Ok(mut source) = EventSource::new(&url) else {
        // A malformed URL is the only failure mode here; nothing is actionable.
        return;
    };
    let Ok(mut subscription) = source.subscribe("notification") else {
        return;
    };

    let mut badge = badge.0;
    while let Some(item) = subscription.next().await {
        // A transient connection error surfaces as an `Err` item; the browser reconnects on
        // its own, so keep awaiting rather than tearing the loop down.
        let Ok((_event, message)) = item else {
            continue;
        };
        let Some(text) = message.data().as_string() else {
            continue;
        };
        if let Ok(push) = serde_json::from_str::<LiveNotification>(&text) {
            badge.set(push.unread_count);
        }
    }

    source.close();
}
