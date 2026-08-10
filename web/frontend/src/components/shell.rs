//! The persistent app shell: left rail + top command bar, with the routed view in the
//! content area. Also owns the two background concerns that must outlive any single screen —
//! the silent token refresh and the live-notification subscription.

use crate::api;
use crate::components::{nav::Rail, topbar::TopBar, BottomTabs, Footer, UnreadBadge};
use crate::state::account_wall::{use_account_wall, Admission};
use crate::state::capabilities::use_capabilities;
use crate::state::use_session;
use crate::Route;
use dioxus::prelude::*;

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
    use_source_order_sync();
    use_unread_count();
    use_live_notifications();
    crate::state::legal::use_legal_sync();
    // Here rather than in each screen: the layout is the one component every route renders
    // through, so no route can be added without a title.
    crate::title::use_document_title();

    let i18n = crate::i18n::use_i18n();
    let route: Route = use_route();
    let walled_out = use_account_wall_redirect(&route);
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
                    // Withheld for the one render between deciding the reader may not be here
                    // and the router arriving at the sign-in screen. Mounting the view anyway
                    // would fire its fetches, every one of which the server refuses.
                    div { class: "ik-measure", if !walled_out { Outlet::<Route> {} } }
                }
                Footer { compact: is_compact(&route) }
            }
            // After `.ik-main`, not inside it: the bar is `position: fixed` at the viewport's
            // bottom edge below 820px and renders to nothing above it.
            BottomTabs {}
        }
    }
}

/// Send a signed-out visitor to the sign-in screen on a deployment that serves nobody else, and
/// report whether this render is one the reader may not see.
///
/// The client half of `accounts.required`. Only the server enforces it — every request is
/// refused with or without this — but a reader left staring at a screen of failed panels has no
/// way to tell a private deployment from a broken one, and no obvious way to the sign-in form.
///
/// Redirecting rather than rendering a "please sign in" card, because the wall is not about the
/// route: on a private deployment *no* address serves this reader, so leaving them on one is
/// leaving them somewhere that cannot ever load.
fn use_account_wall_redirect(route: &Route) -> bool {
    let session = use_session();
    let wall = use_account_wall();
    // No `is_settled` check here: the wall is only ever raised by a probe that already waited
    // for the boot-time refresh, so it cannot be up while "signed out" still means "we have not
    // looked yet".
    let walled_out = wall.is_up() && !session.is_authenticated() && !is_reachable_signed_out(route);

    use_effect(use_reactive!(|walled_out| {
        if walled_out {
            // `replace`, not `push`: the address the reader arrived at is one this deployment
            // has no signed-out answer for, so it does not belong in their history.
            navigator().replace(Route::Login {});
        }
    }));

    walled_out
}

/// The screens a signed-out visitor may still reach while the deployment is private.
///
/// The mirror of `is_sign_in_surface` in `services/api/src/account_gate.rs`, and it has to stay
/// one: a screen listed here whose data the server walls renders empty forever, and a screen
/// missing from here that the server *does* serve is one the reader is bounced away from. There
/// is no compile-time relationship between the two — `openapi.json` connects the workspaces, and
/// it carries routes, not screens — so the pairing is this comment and the tests below it.
fn is_reachable_signed_out(route: &Route) -> bool {
    matches!(
        route,
        Route::Login {}
            | Route::ForgotPassword {}
            | Route::ResetPassword { .. }
            | Route::VerifyEmail { .. }
            // Registering is the act of accepting the Terms, so the documents behind that link
            // have to open for someone who has not registered yet.
            | Route::Legal { .. }
    )
}

/// Whether this route gets the one-line footer instead of the five-column one.
///
/// The auth card and the console are both surfaces a full directory would outweigh — a 400px
/// sign-in card with three columns of links under it, an operator console that is an
/// application rather than a document. Chosen here rather than by the views so a route cannot
/// end up rendering two footers, which is what happened when the auth view supplied its own.
fn is_compact(route: &Route) -> bool {
    matches!(
        route,
        Route::Login {}
            | Route::VerifyEmail { .. }
            | Route::ForgotPassword {}
            | Route::ResetPassword { .. }
            | Route::Console {}
    )
}

/// The measured column width for a route (layout handoff §2.1).
///
/// A grid of covers and a paragraph of prose do not want the same width: the cover screens buy
/// three or four more covers per row, the ledgers stay scannable, and the panel/prose screens
/// stop stretching a 64ch paragraph across a 1600px column. `none` is a real `max-width`, which
/// is how the console keeps its full-bleed opt-out.
fn measure_for(route: &Route) -> &'static str {
    match route {
        Route::Home {} | Route::Discover { .. } | Route::Search { .. } => "1760px",
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
                    crate::platform::sleep_ms(SIGNED_OUT_POLL_MS).await;
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
            crate::platform::sleep_ms(wait_ms.max(0.0) as u32).await;

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
                        crate::platform::sleep_ms(backoff_ms).await;
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
///
/// While signed *out* the same probe answers a different question: whether this deployment
/// serves anonymous callers at all. Its refusal is the only channel that carries the answer —
/// `account_required` rather than `unauthorized` — because a deployment behind
/// `accounts.required` publishes nothing to a caller with no account, this endpoint included.
/// Held back until the session has settled: before the boot-time silent refresh lands, "signed
/// out" only means "we have not looked yet", and probing then would raise the wall in front of a
/// reader who is a moment away from being signed in.
fn use_capability_sync() {
    let session = use_session();
    let api = api::use_api();
    let capabilities = use_capabilities();
    let wall = use_account_wall();

    use_resource(move || {
        let client = api.client();
        let signed_in = session.is_authenticated();
        let settled = session.is_settled();
        async move {
            if !signed_in {
                capabilities.clear();
                if settled {
                    if let Err(error) = client.capabilities().send().await {
                        // A transport fault yields `None` and leaves the previous answer alone:
                        // an unreachable server is not evidence of an admission policy.
                        if let Some(admission) = api::admission(&error) {
                            wall.set(admission);
                        }
                    }
                }
                return;
            }
            // An account is through the wall by definition, so nothing here observes it; a
            // sign-out re-runs this hook and the probe above answers again.
            wall.set(Admission::Unknown);
            match client.capabilities().send().await {
                Ok(response) => capabilities.set(response.into_inner()),
                Err(_) => capabilities.clear(),
            }
        }
    });
}

/// Keep the cached global source order in step with the session.
///
/// Keyed on the token for the same reason capabilities are: the order is per reader, so a
/// sign-out must not leave the previous reader's preference shaping this one's links.
fn use_source_order_sync() {
    let session = use_session();
    let api = api::use_api();
    let order = crate::state::source_order::use_source_order();

    use_resource(move || {
        let client = api.client();
        let signed_in = session.is_authenticated();
        async move {
            if !signed_in {
                order.clear();
                return;
            }
            match client.source_preferences().send().await {
                Ok(response) => order.set(
                    response
                        .into_inner()
                        .providers
                        .into_iter()
                        .map(|p| p.slug)
                        .collect(),
                ),
                Err(_) => order.clear(),
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
    // Threaded in rather than read inside the stream: the desktop build words its OS
    // notification through the catalogue, and `use_i18n` is a hook — it cannot be called from
    // inside a future that has already awaited.
    let i18n = crate::i18n::use_i18n();

    use_resource(move || {
        let signed_in = session.is_authenticated();
        async move {
            if signed_in {
                crate::live::run(api, badge, i18n).await;
            }
        }
    });
}

/// Seed the unread badge from the server, and re-seed it on every token change.
///
/// The stream alone was not enough, and this is the defect that made the count "not always
/// load": the SSE stream only ever *pushes a change*. A reader who opens the app with unread
/// notifications and receives nothing new while the tab is open gets no push at all, so the
/// badge sat at its initial zero until they happened to visit `/notifications`, which is the one
/// screen that recounts. On a reload the same thing happened again.
///
/// Keyed on the token like the stream and the capability sync, so the count is fetched on
/// sign-in, on the boot-time silent refresh, and on each renewal — the cadence that also
/// repairs a badge the stream drifted away from during a disconnection.
///
/// `limit(1)`: the inbox-wide `unread` total is a field of the response, not something counted
/// from the rows, so there is no reason to transfer a page of them to read it. A failed fetch
/// deliberately leaves the previous count alone — the badge is a nicety, and blanking it on a
/// transient error would report "nothing unread", which is a specific and wrong claim.
fn use_unread_count() {
    let session = use_session();
    let api = api::use_api();
    let badge = use_context::<UnreadBadge>();

    use_resource(move || {
        let client = api.client();
        let signed_in = session.is_authenticated();
        let mut count = badge.0;
        async move {
            if !signed_in {
                count.set(0);
                return;
            }
            if let Ok(response) = client.notifications().limit(1).offset(0).send().await {
                count.set(response.into_inner().unread);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::{is_reachable_signed_out, Route};
    use crate::views::{ConsoleQuery, DiscoverQuery, SearchQuery, WatchlistQuery};

    /// Every screen the API's `account_gate` also lets through, so a private deployment still
    /// has a way in. A screen dropped from this list is a reader bounced off the very form they
    /// were sent a link to — the password reset and the email confirmation both arrive by email
    /// at an address whose owner is, by definition, signed out.
    #[test]
    fn the_way_in_is_reachable_signed_out() {
        for route in [
            Route::Login {},
            Route::ForgotPassword {},
            Route::ResetPassword {
                token: "t".to_owned(),
            },
            Route::VerifyEmail {
                token: "t".to_owned(),
            },
            Route::Legal {
                slug: "terms".to_owned(),
            },
        ] {
            assert!(
                is_reachable_signed_out(&route),
                "{route} is part of getting an account and must survive the wall"
            );
        }
    }

    /// The inverse leg: a predicate that answered `true` too readily would leave every screen
    /// mounted, each one fetching data the server refuses, on a deployment whose whole point is
    /// that it serves none of it.
    #[test]
    fn everything_a_reader_comes_for_is_behind_the_wall() {
        for route in [
            Route::Home {},
            Route::Discover {
                query: DiscoverQuery::default(),
            },
            Route::Search {
                query: SearchQuery::default(),
            },
            Route::Series {
                id: "id".to_owned(),
            },
            Route::Watchlist {
                query: WatchlistQuery::default(),
            },
            Route::Notifications {},
            Route::Account {},
            Route::Console {},
            Route::NotFound {
                segments: Vec::new(),
            },
        ] {
            assert!(
                !is_reachable_signed_out(&route),
                "{route} must not be reachable on a deployment that requires an account"
            );
        }
    }

    /// `ConsoleQuery` is in scope for the operator route above; naming it keeps the import
    /// honest if that list ever changes.
    #[test]
    fn the_console_section_is_behind_the_wall_too() {
        assert!(!is_reachable_signed_out(&Route::ConsoleSection {
            entity: crate::views::ConsoleEntity::Overview,
            query: ConsoleQuery::fresh(),
        }));
    }
}
