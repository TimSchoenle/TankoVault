//! TankoVault frontend — Dioxus (WASM SPA) + the Inkstone design system (design §17).
//!
//! A reader-facing SPA plus an operator console. Client state via Dioxus signals; server
//! state via `use_resource` fetch-caches; auth token in memory (refreshed via the httpOnly
//! cookie on boot). Routes are type-safe via `dioxus-router`.

// Component functions are intentionally PascalCase; `#[component]` handles the lint locally,
// but the route table also names them, so keep the allow crate-wide for clarity.
#![allow(non_snake_case)]

mod api;
mod components;
mod icons;
mod live;
mod models;
mod state;
mod views;
mod wire;

use components::{Shell, UnreadBadge};
use dioxus::prelude::*;
use state::Session;
use views::{
    Account, AnilistCallback, Console, Discover, Home, Login, NotFound, Notifications, Search,
    Series, Watchlist,
};

/// The type-safe route table (design §17.4). All screens live under the persistent `Shell`
/// layout (left rail + top command bar).
#[derive(Routable, Clone, PartialEq)]
#[rustfmt::skip]
pub enum Route {
    #[layout(Shell)]
        #[route("/")]
        Home {},
        #[route("/discover")]
        Discover {},
        #[route("/series/:id")]
        Series { id: String },
        // Old Reading feed folded into Home; keep the path alive for bookmarks/links.
        #[redirect("/reading", || Route::Home {})]
        #[route("/watchlist")]
        Watchlist {},
        #[route("/notifications")]
        Notifications {},
        #[route("/account")]
        Account {},
        // `AniList` OAuth redirect target (sync service `redirect_uri`); reads `?code=` and
        // exchanges it via the Bearer-authenticated `/v1/me/sync/anilist/callback`.
        #[route("/account/anilist-callback?:code")]
        AnilistCallback { code: String },
        #[route("/search?:q")]
        Search { q: String },
        #[route("/login")]
        Login {},
        #[route("/console")]
        Console {},
        #[route("/:..segments")]
        NotFound { segments: Vec<String> },
}

fn main() {
    dioxus::launch(App);
}

#[component]
fn App() -> Element {
    // App-wide contexts: the session (token + role), the unread-notification badge,
    // and the typed API client.
    use_context_provider(Session::new);
    use_context_provider(|| UnreadBadge(Signal::new(0)));
    crate::api::provide_api();

    rsx! {
        document::Stylesheet { href: asset!("/assets/main.css") }
        FontFaces {}
        Router::<Route> {}
    }
}

/// Self-hosted font subsets (DESIGN_SPEC §3.3), wired through the Dioxus asset system so the
/// `.woff2` files are bundled and content-hashed in the release build (a plain `url()` in the
/// Tailwind-built CSS is *not* processed by manganis, so it would 404 in release). Emitting the
/// `@font-face` rules here with `asset!()`-resolved URLs makes Bricolage Grotesque / IBM Plex
/// Sans / IBM Plex Mono load in both `dx serve` and the hashed release bundle. `font-display:
/// swap` keeps text visible while a subset loads; the system stacks in `input.css` are the
/// fallback.
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
