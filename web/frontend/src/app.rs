//! The app root: the route table, the contexts every screen depends on, and the bundled
//! font faces.

#[cfg(feature = "desktop")]
use crate::components::{CloseToTray, SettingsSheet, TitleBar, TrayHost};
use crate::components::{FocusTargets, Shell, UnreadBadge};
use crate::i18n::I18nRoot;
use crate::state::account_wall::AccountWall;
use crate::state::branding::BrandingState;
use crate::state::capabilities::CapabilitySet;
use crate::state::legal::LegalIndex;
use crate::state::source_order::SourceOrder;
use crate::state::Session;
use crate::title::PageTitle;
use crate::views::{
    Account, AnilistCallback, Console, ConsoleEntity, ConsoleQuery, ConsoleSection, Discover,
    DiscoverQuery, ForgotPassword, Home, Legal, Login, NotFound, Notifications, Recommendations,
    ResetPassword, Search, SearchQuery, Series, VerifyEmail, Watchlist, WatchlistQuery,
};
use dioxus::prelude::*;

/// The type-safe route table (design §17.4). Every screen lives under the persistent [`Shell`]
/// layout (left rail + top command bar).
#[derive(Routable, Clone, PartialEq)]
#[rustfmt::skip]
pub(crate) enum Route {
    #[layout(Shell)]
        #[route("/")]
        Home {},
        // Filters, sort *and* how far the reader had scrolled ride in the query string, so a
        // shared link opens on the same covers; see `views::discover::query`.
        #[route("/discover?:..query")]
        Discover { query: DiscoverQuery },
        // Its own screen rather than a shelf on Home: the reasons are the surface, and a rail
        // under three other sections had room for neither the list nor the explanations.
        #[route("/for-you")]
        Recommendations {},
        #[route("/series/:id")]
        Series { id: String },
        // The old Reading feed folded into Home; the path stays alive for bookmarks and links.
        #[redirect("/reading", || Route::Home {})]
        // View state (tab/sort/filter/etc.) rides in the query string so a filtered watchlist
        // is shareable and back-button-able; see `views::watchlist::query`.
        #[route("/watchlist?:..query")]
        Watchlist { query: WatchlistQuery },
        #[route("/notifications")]
        Notifications {},
        #[route("/account")]
        Account {},
        // AniList's OAuth redirect target (the sync service's `redirect_uri`); reads `?code=`
        // and exchanges it via the bearer-authenticated callback endpoint.
        #[route("/account/anilist-callback?:code")]
        AnilistCallback { code: String },
        // The term *and* its options ride in the query string, so a search worth sending to
        // someone arrives narrowed the way its sender narrowed it; see `views::search::query`.
        #[route("/search?:..query")]
        Search { query: SearchQuery },
        #[route("/login")]
        Login {},
        // Email-confirmation and password-reset landing pages; the token rides in the query
        // string of the link sent by email.
        #[route("/verify-email?:token")]
        VerifyEmail { token: String },
        #[route("/forgot-password")]
        ForgotPassword {},
        #[route("/reset-password?:token")]
        ResetPassword { token: String },
        // `/console` is the way in: it resolves the operator's last entity and replaces itself
        // with the addressable form below, so no console view an operator reaches is unlinkable.
        #[route("/console")]
        Console {},
        #[route("/console/:entity?:..query")]
        ConsoleSection { entity: ConsoleEntity, query: ConsoleQuery },
        // An entity slug this build has dropped lands in the console rather than on a 404 — the
        // rail is capability-filtered anyway, so "no such entity" and "not for you" already
        // resolve the same way.
        #[redirect("/console/:_entity", |_entity: String| Route::Console {})]
        // Operator-published documents. The slug set is configuration, not code — an
        // operator can publish one this build has never heard of — so the segment is a
        // free string and an unconfigured one is the API's 404, not a routing miss.
        #[route("/legal/:slug")]
        Legal { slug: String },
        #[route("/:..segments")]
        NotFound { segments: Vec<String> },
}

#[component]
pub(crate) fn App() -> Element {
    // Order matters: the API handle reads the session for the live bearer token, so the
    // session context has to exist first.
    use_context_provider(Session::new);
    // Starts empty and is filled once `Shell` syncs capabilities; provided outside the router
    // so views don't need it threaded down.
    use_context_provider(CapabilitySet::new);
    // Whether this deployment serves signed-out visitors at all. Observed by the same probe as
    // the capabilities above, and read by `Shell` to decide whether a signed-out reader belongs
    // anywhere but the sign-in screen.
    use_context_provider(AccountWall::new);
    use_context_provider(|| UnreadBadge(Signal::new(0)));
    // The reader's global source order; filled by `Shell` and re-set by the Account panel that
    // edits it, so a save lands on the Series screen without a reload.
    use_context_provider(SourceOrder::new);
    // Filled once by `Shell`; three surfaces read the same list.
    use_context_provider(LegalIndex::new);
    // The shipped identity until `Shell` fetches this deployment's. Provided here, above
    // `I18nRoot`, because the translator substitutes the product name into every message that
    // names it — so it has to be in scope wherever a message is rendered.
    use_context_provider(BrandingState::new);
    // Empty until a screen publishes a name only it knows; the route's own name covers the rest.
    use_context_provider(PageTitle::new);
    // Registered on mount by the two fields, read by the shortcut that focuses them.
    use_context_provider(FocusTargets::new);
    // The short-lived elevation the sensitive screens present. In memory only, like the access
    // token, and for the same reason — see `state::step_up`.
    crate::state::step_up::provide_step_up();
    crate::api::provide_api();

    // Applies the stored appearance choices to the document root. A no-op on web beyond
    // re-asserting what the boot script in `index.html` already wrote before first paint; on
    // desktop there is no boot script, so this *is* how the reader's theme gets applied.
    use_hook(crate::state::prefs::hydrate_appearance);

    rsx! {
        document::Stylesheet { href: asset!("/assets/main.css") }
        FontFaces {}
        // Above the router so a language change can re-render every screen.
        I18nRoot {
            AppRoot {
                Connected {}
            }
        }
    }
}

/// The router, once there is a server to point it at.
///
/// Desktop starts with no origin at all, and every screen behind the router issues requests, so
/// the choice has to be made before any of them mounts — not inside a route they could navigate
/// away from.
#[cfg(feature = "desktop")]
#[component]
fn Connected() -> Element {
    let api = crate::api::use_api();
    let mut origin = use_signal(crate::platform::server_origin);

    if origin.read().is_none() {
        return rsx! {
            crate::views::ConnectServer {
                on_connected: move |chosen: String| {
                    api.set_base(&chosen);
                    origin.set(Some(chosen));
                },
            }
        };
    }
    rsx! { Router::<Route> {} }
}

#[cfg(feature = "web")]
#[component]
fn Connected() -> Element {
    rsx! { Router::<Route> {} }
}

/// The element the appearance attributes live on for the desktop build, and a plain pass-through
/// on web.
///
/// Web writes `data-theme` and friends onto `<html>`, which Rust can reach there and cannot
/// reach in a wry webview — see `crate::platform::desktop`. The stylesheet's rules are bare
/// attribute selectors (`[data-theme="light"]`, not `:root[data-theme="light"]`), so they apply
/// from here just as well.
///
/// It also carries the page fill; `.ik-desktop-root` in `input.css` says why, and why the
/// formatting context it establishes is load-bearing rather than tidying.
///
/// A named class rather than the inline `style:` this codebase usually reaches for: the Tailwind
/// CLI scans these sources for class names, and `display:flow-root` in a style string is enough
/// for it to mint a phantom `.flow-root` utility nothing renders.
#[cfg(feature = "desktop")]
#[component]
fn AppRoot(children: Element) -> Element {
    let attributes = crate::platform::ROOT_ATTRIBUTES.read().clone();
    let mut settings_open = use_signal(|| false);

    // Provided here rather than in `App` because it is desktop-only, and read by two things that
    // both live at this level: the title bar's dot and the settings sheet. The loop it feeds waits
    // out first paint on its own and then checks every six hours — see `crate::update`.
    let update = use_context_provider(crate::update::UpdateState::new);
    let i18n = crate::i18n::use_i18n();
    // Before the loop, so a client that has just replaced itself says so rather than reporting
    // "no check has run yet" about a version it acquired thirty seconds ago.
    use_hook(|| crate::update::adopt_applied(update, i18n));
    use_future(move || crate::update::run(update, i18n));

    // Whether the close button ends the app or hides it. Provided here because two things read
    // it — the sheet that sets it and the `TrayHost` that makes it true — and neither owns the
    // other.
    use_context_provider(CloseToTray::new);

    // Once, on the first render: size the window to the display the builder could not know about
    // — a laptop at 1366×768 would otherwise keep the 1280×860 placeholder, taller than its own
    // screen, with the footer and the sign-in button below the bottom edge.
    //
    // The UI is held back until it resolves, and that gate is the fix for a real defect rather
    // than caution. The window opens at `STARTUP_INNER_SIZE` and is resized a moment later, so a
    // screen that mounted in between measured a geometry the reader never sees — and Discover
    // turns its measurement into a page size (`components::use_grid_fit`), so it fetched a
    // 1280px window's worth of covers and then laid them into the fitted window's wider grid,
    // one short row per page. The browser has no equivalent: its viewport is final before first
    // paint, which is why this only ever went wrong in the native client.
    let mut fitted = use_signal(|| false);
    let desktop = crate::platform::window();
    use_future(move || {
        let desktop = desktop.clone();
        async move {
            crate::platform::fit_window_to_display(desktop).await;
            fitted.set(true);
        }
    });

    // The OS caption is off, so the window's light/dark chrome — its shadow, its resize borders,
    // and the caption itself if decorations are ever turned back on — has to be told which theme
    // the *app* is in. Left alone it follows the system's, which is how a dark border ended up
    // around the Warm Paper theme.
    let theme = attributes.get("data-theme");
    use_effect(move || {
        if let Some(window) = crate::platform::window() {
            window.set_theme(Some(match theme.as_deref() {
                Some("light") => dioxus::desktop::tao::window::Theme::Light,
                _ => dioxus::desktop::tao::window::Theme::Dark,
            }));
        }
    });

    rsx! {
        div {
            class: "ik-desktop-root",
            lang: attributes.get("lang"),
            "data-theme": attributes.get("data-theme"),
            "data-accent": attributes.get("data-accent"),
            "data-density": attributes.get("data-density"),
            "data-cover": attributes.get("data-cover"),
            TitleBar {
                on_settings: move |()| settings_open.set(true),
                update_waiting: update.wants_attention(),
            }
            // Renders nothing; it owns the tray icon's lifetime and the close button's meaning.
            TrayHost {}
            // Empty until the window has been fitted; see the gate above. The title bar renders
            // either way, so the frame is never a bare rectangle.
            div { class: "ik-desktop-body", if fitted() { {children} } }
            if settings_open() {
                SettingsSheet { on_close: move |()| settings_open.set(false) }
            }
        }
    }
}

#[cfg(feature = "web")]
#[component]
fn AppRoot(children: Element) -> Element {
    rsx! { {children} }
}

/// Self-hosted font subsets (`DESIGN_SPEC` §3.3), wired through the Dioxus asset system so the
/// `.woff2` files are bundled and content-hashed in the release build.
///
/// Emitted from Rust rather than `input.css`: manganis doesn't rewrite `url()` inside the
/// Tailwind-built stylesheet, so a plain `url(fonts/…)` there 404s in the hashed release build.
#[component]
fn FontFaces() -> Element {
    const BRICOLAGE: Asset = asset!("/assets/fonts/bricolage-grotesque-latin-variable-wght.woff2");
    const SANS_400: Asset = asset!("/assets/fonts/ibm-plex-sans-latin-400-normal.woff2");
    const SANS_500: Asset = asset!("/assets/fonts/ibm-plex-sans-latin-500-normal.woff2");
    const SANS_600: Asset = asset!("/assets/fonts/ibm-plex-sans-latin-600-normal.woff2");
    const SANS_700: Asset = asset!("/assets/fonts/ibm-plex-sans-latin-700-normal.woff2");
    const MONO_400: Asset = asset!("/assets/fonts/ibm-plex-mono-latin-400-normal.woff2");
    const MONO_500: Asset = asset!("/assets/fonts/ibm-plex-mono-latin-500-normal.woff2");
    const MONO_600: Asset = asset!("/assets/fonts/ibm-plex-mono-latin-600-normal.woff2");

    let face = |family: &str, weight: &str, src: &Asset| {
        format!(
            "@font-face{{font-family:\"{family}\";font-style:normal;font-weight:{weight};\
             font-display:swap;src:url({src}) format(\"woff2\");}}"
        )
    };
    let css = [
        face("Bricolage Grotesque", "400 800", &BRICOLAGE),
        face("IBM Plex Sans", "400", &SANS_400),
        face("IBM Plex Sans", "500", &SANS_500),
        face("IBM Plex Sans", "600", &SANS_600),
        face("IBM Plex Sans", "700", &SANS_700),
        face("IBM Plex Mono", "400", &MONO_400),
        face("IBM Plex Mono", "500", &MONO_500),
        face("IBM Plex Mono", "600", &MONO_600),
    ]
    .concat();

    rsx! {
        document::Style { {css} }
    }
}
