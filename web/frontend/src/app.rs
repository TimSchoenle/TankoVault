//! The app root: the route table, the contexts every screen depends on, and the bundled
//! font faces.

use crate::components::{FocusTargets, Shell, UnreadBadge};
#[cfg(feature = "desktop")]
use crate::components::{SettingsSheet, TitleBar};
use crate::i18n::I18nRoot;
use crate::state::capabilities::CapabilitySet;
use crate::state::legal::LegalIndex;
use crate::state::source_order::SourceOrder;
use crate::state::Session;
use crate::title::PageTitle;
use crate::views::{
    Account, AnilistCallback, Console, ConsoleEntity, ConsoleQuery, ConsoleSection, Discover,
    DiscoverQuery, ForgotPassword, Home, Legal, Login, NotFound, Notifications, Recommendations,
    ResetPassword, Search, Series, VerifyEmail, Watchlist, WatchlistQuery,
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
        #[route("/search?:q")]
        Search { q: String },
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
    use_context_provider(|| UnreadBadge(Signal::new(0)));
    // The reader's global source order; filled by `Shell` and re-set by the Account panel that
    // edits it, so a save lands on the Series screen without a reload.
    use_context_provider(SourceOrder::new);
    // Filled once by `Shell`; three surfaces read the same list.
    use_context_provider(LegalIndex::new);
    // Empty until a screen publishes a name only it knows; the route's own name covers the rest.
    use_context_provider(PageTitle::new);
    // Registered on mount by the two fields, read by the shortcut that focuses them.
    use_context_provider(FocusTargets::new);
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
    use_future(move || crate::update::run(update, i18n));

    // Once, on the first render: shrink the window to the display if the default does not fit.
    // The builder cannot do this — it runs before there is an event loop to ask which monitor
    // the window landed on — so a laptop at 1366×768 would otherwise open a 1280×860 window
    // taller than its screen, with the footer and the sign-in button below the bottom edge.
    use_hook(crate::platform::fit_window_to_display);

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
            div { class: "ik-desktop-body", {children} }
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
