//! The persistent app shell: left rail + top command bar, with the routed view in the
//! content area. Also owns the two background concerns that must outlive any single screen —
//! the silent token refresh and the live-notification subscription.

use crate::api;
use crate::components::{nav::Rail, topbar::TopBar, UnreadBadge};
use crate::state::use_session;
use crate::Route;
use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;

/// How long before the access token's `exp` the background refresh fires. Comfortably inside
/// the server's 15-minute default `access_ttl_minutes`.
const REFRESH_BUFFER_MS: f64 = 60_000.0;

/// Poll cadence while signed out — there is no cookie to adopt yet, so we just check back.
const SIGNED_OUT_POLL_MS: u32 = 15_000;

/// First delay before retrying a *transient* refresh failure.
const RETRY_BACKOFF_START_MS: u32 = 2_000;

/// Ceiling for the transient-failure backoff, so a prolonged outage settles into a steady,
/// low-frequency retry rather than growing unbounded.
const RETRY_BACKOFF_MAX_MS: u32 = 60_000;

#[component]
pub(crate) fn Shell() -> Element {
    use_token_refresh();
    use_live_notifications();

    rsx! {
        div { class: "ik-app",
            Rail {}
            main { class: "ik-main",
                TopBar {}
                section { class: "ik-content", Outlet::<Route> {} }
            }
        }
    }
}

/// Keep an access token in memory for as long as the tab is open (design §17.4).
///
/// Runs once on boot — adopting a token from the httpOnly refresh cookie so a page reload
/// stays signed in — and then again shortly before each token expires.
///
/// The recurring half is not optional. Without it the in-memory token goes stale ~15 minutes
/// after boot and every authenticated call starts 401ing until the user manually reloads.
/// The SSE stream suffers worst: `EventSource` bakes the token into its URL and, per spec,
/// stops reconnecting for good the first time a reconnect attempt draws a non-200 — so one
/// stale-token 401 kills live notifications permanently. Refreshing ahead of expiry keeps the
/// session token current, and because the stream below is keyed on it, the connection is
/// transparently re-opened with a valid token before that can happen.
///
/// Crucially, a *failed* refresh is not a sign-out. Only a genuine `401` — the refresh
/// session really is gone: expired past its 30-day window, rotated away, or reuse-revoked —
/// clears the session. Everything else (offline, DNS, timeout, 5xx, a server restart, a
/// laptop waking from sleep) is transient, the httpOnly cookie is still valid, and we retry
/// with exponential backoff. That distinction is what makes reloads and brief outages behave
/// like a normal site instead of bouncing the reader to the login screen.
fn use_token_refresh() {
    let session = use_session();
    let api = api::use_api();

    use_future(move || async move {
        // Grows on consecutive transient failures, resets on any definitive answer.
        let mut backoff_ms = RETRY_BACKOFF_START_MS;
        loop {
            let booted = *session.ready.peek();
            // The two `0.0` arms are not mergeable: the guarded `None if booted` arm has to
            // sit between them, and an or-pattern would swallow it.
            #[allow(clippy::match_same_arms)]
            let wait_ms = match session.expires_in_ms() {
                Some(ms) if ms > REFRESH_BUFFER_MS => ms - REFRESH_BUFFER_MS,
                // Already inside the buffer (or past expiry): refresh immediately.
                Some(_) => 0.0,
                // No token and we have already booted: either signed out and waiting for a
                // sign-in, or a transient boot failure left us tokenless. Poll so a later
                // sign-in — or a recovered server — is picked up, without hammering.
                None if booted => {
                    TimeoutFuture::new(SIGNED_OUT_POLL_MS).await;
                    continue;
                }
                None => 0.0,
            };
            // The wait is bounded by the token's TTL (minutes), so it always fits `u32`;
            // a negative value only arises for an already-expired token, where clamping to
            // zero is exactly the wanted behaviour.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            TimeoutFuture::new(wait_ms.max(0.0) as u32).await;

            // The refresh endpoint is cookie-authenticated, but it still needs a client with
            // the real same-origin base URL: reqwest rejects a relative path outright.
            match api.client().refresh().send().await {
                Ok(response) => {
                    session.set_token(response.into_inner().access_token);
                    backoff_ms = RETRY_BACKOFF_START_MS;
                }
                Err(e) if e.status() == Some(reqwest::StatusCode::UNAUTHORIZED) => {
                    session.clear();
                    backoff_ms = RETRY_BACKOFF_START_MS;
                }
                Err(_) => {
                    // On boot we skip the sleep and fall through to `mark_ready` so the UI
                    // still paints promptly; the `None if booted` poll drives recovery.
                    if booted {
                        TimeoutFuture::new(backoff_ms).await;
                        backoff_ms = backoff_ms.saturating_mul(2).min(RETRY_BACKOFF_MAX_MS);
                    }
                }
            }
            if !booted {
                session.mark_ready();
            }
        }
    });
}

/// Subscribe to the per-user SSE stream while signed in, keeping the rail's unread badge
/// current (design §14, §17.4).
///
/// `use_resource` restarts when the token changes — dropping the previous `EventSource` and
/// closing its connection — so a sign-out or a silent refresh transparently tears the stream
/// down or re-establishes it.
fn use_live_notifications() {
    let session = use_session();
    let api = api::use_api();
    let badge = use_context::<UnreadBadge>();

    use_resource(move || {
        let token = session.token_value();
        async move {
            if let Some(token) = token {
                crate::live::run(api, token, badge).await;
            }
        }
    });
}
