//! The app root: the route table, the contexts every screen depends on, and the bundled
//! font faces.

use crate::components::{Shell, UnreadBadge};
use crate::i18n::I18nRoot;
use crate::state::capabilities::CapabilitySet;
use crate::state::Session;
use crate::title::PageTitle;
use crate::views::{
    Account, AnilistCallback, Console, Discover, ForgotPassword, Home, Login, NotFound,
    Notifications, ResetPassword, Search, Series, VerifyEmail, Watchlist, WatchlistQuery,
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
        #[route("/discover")]
        Discover {},
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
        #[route("/console")]
        Console {},
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
    // Empty until a screen publishes a name only it knows; the route's own name covers the rest.
    use_context_provider(PageTitle::new);
    crate::api::provide_api();

    rsx! {
        document::Stylesheet { href: asset!("/assets/main.css") }
        FontFaces {}
        // Above the router so a language change can re-render every screen.
        I18nRoot {
            Router::<Route> {}
        }
    }
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
