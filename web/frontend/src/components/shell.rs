//! The persistent app shell: left rail + top command bar, with the routed view in the
//! content area. Also owns the two background concerns that must outlive any single screen —
//! the silent token refresh and the live-notification subscription.

use crate::api;
use crate::components::{nav::Rail, topbar::TopBar, BottomTabs, UnreadBadge};
use crate::state::capabilities::use_capabilities;
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
    use_capability_sync();
    use_live_notifications();
    crate::state::legal::use_legal_sync();
    // Here rather than in each screen: the layout is the one component every route renders
    // through, so no route can be added without a title.
    crate::title::use_document_title();

    let i18n = crate::i18n::use_i18n();
    let route: Route = use_route();
    rsx! {
        div { class: "ik-app",
            // Skip link: without it, a keyboard reader re-tabs the ~10-stop rail on every route.
            a { class: "ik-skip", href: "#ik-content", {i18n.t("nav.skipToContent")} }
            Rail {}
            // `--measure` lands on `.ik-main`, not on the view root: the top bar and the footer
            // are siblings of the content, so a value set inside the content could not reach
            // them, and a chrome row measured differently from the list it acts on is the
            // defect this layout exists to fix.
            main { class: "ik-main", style: "--measure:{measure_for(&route)};",
                TopBar {}
                section { id: "ik-content", class: "ik-content",
                    div { class: "ik-measure", Outlet::<Route> {} }
                }
            }
            // After `.ik-main`, not inside it: the bar is `position: fixed` at the viewport's
            // bottom edge below 820px and renders to nothing above it.
            BottomTabs {}
        }
    }
}

/// The measured column width for a route (layout handoff §2.1).
///
/// A grid of covers and a paragraph of prose do not want the same width: the cover screens buy
/// three or four more covers per row, the ledgers stay scannable, and the panel/prose screens
/// stop stretching a 64ch paragraph across a 1600px column. `none` is a real `max-width`, which
/// is how the console keeps its full-bleed opt-out.
fn measure_for(route: &Route) -> &'static str {
    match route {
        Route::Home {} | Route::Discover {} | Route::Search { .. } => "1760px",
        Route::Account {} | Route::AnilistCallback { .. } | Route::Legal { .. } => "1120px",
        Route::Console {} => "none",
        _ => "1600px",
    }
}

/// Keep an access token in memory for as long as the tab is open (design §17.4).
///
/// Runs once on boot — adopting a token from the httpOnly refresh cookie so a page reload
/// stays signed in — and then again shortly before each token expires.
///
/// The recurring half is not optional. Without it the in-memory token goes stale ~15 minutes
/// after boot and every authenticated call starts 401ing until the user manually reloads.
/// The SSE stream used to suffer worst: `EventSource` baked the access token into its URL and, per
/// spec, stops reconnecting for good the first time a reconnect attempt draws a non-200 — so one
/// stale-token 401 killed live notifications permanently. Since SEC-8 the stream authenticates with
/// a ticket it mints per attempt and drives its own reconnect (`crate::live`), so it no longer
/// depends on the token staying fresh — but every other authenticated call still does.
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
            #[expect(
                clippy::match_same_arms,
                reason = "the guarded `None if booted` arm sits between the two `0.0` arms, so an \
                          or-pattern would swallow it"
            )]
            let wait_ms = match session.expires_in_ms() {
                Some(ms) if ms > REFRESH_BUFFER_MS => ms - REFRESH_BUFFER_MS,
                // Already inside the buffer (or past expiry): refresh immediately.
                Some(_) => 0.0,
                // Signed out, or a transient boot failure left us tokenless: poll without hammering.
                None if booted => {
                    TimeoutFuture::new(SIGNED_OUT_POLL_MS).await;
                    continue;
                }
                None => 0.0,
            };
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "the wait is bounded by the token TTL (minutes) so it fits u32, and \
                          `max(0.0)` clamps an already-expired token to an immediate refresh"
            )]
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

/// Keep the reader's capabilities — their permissions and this deployment's enabled features —
/// in step with the session.
///
/// Keyed on the access token via `use_resource`, exactly like the live stream below, so it
/// refetches on sign-in, on the boot-time silent refresh, and on every renewal. That cadence is
/// what makes a permission granted or revoked while someone is signed in reach their UI within
/// one token lifetime rather than never.
///
/// A failed fetch clears rather than keeps the previous answer: a stale capability set is how a
/// reader ends up looking at a console tab they no longer have, and showing less than they are
/// entitled to is the recoverable direction — the next refresh puts it back.
fn use_capability_sync() {
    let session = use_session();
    let api = api::use_api();
    let capabilities = use_capabilities();

    use_resource(move || {
        let client = api.client();
        let signed_in = session.is_authenticated();
        async move {
            if !signed_in {
                capabilities.clear();
                return;
            }
            match client.capabilities().send().await {
                Ok(response) => capabilities.set(response.into_inner()),
                Err(_) => capabilities.clear(),
            }
        }
    });
}

/// Subscribe to the per-user SSE stream while signed in, keeping the rail's unread badge
/// current (design §14, §17.4).
///
/// `use_resource` restarts when the token changes — dropping the previous `EventSource` and
/// closing its connection — so a sign-out or a silent refresh transparently tears the stream
/// down or re-establishes it. The token is read only to decide *whether* to run and to key the
/// resource; the stream authenticates with a single-use ticket [`crate::live::run`] mints for
/// itself, so it is never in the URL (SEC-8).
fn use_live_notifications() {
    let session = use_session();
    let api = api::use_api();
    let badge = use_context::<UnreadBadge>();

    use_resource(move || {
        let signed_in = session.is_authenticated();
        async move {
            if signed_in {
                crate::live::run(api, badge).await;
            }
        }
    });
}
