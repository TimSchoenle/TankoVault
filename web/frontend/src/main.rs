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
mod live;
mod models;
mod state;
mod views;

use components::{Shell, UnreadBadge};
use dioxus::prelude::*;
use state::Session;
use views::{
    Console, Discover, Login, NotFound, Notifications, Reading, Search, Series, Watchlist,
};

/// The type-safe route table (design §17.4). All screens live under the persistent `Shell`
/// layout (left rail + top command bar).
#[derive(Routable, Clone, PartialEq)]
#[rustfmt::skip]
pub enum Route {
    #[layout(Shell)]
        #[route("/")]
        Discover {},
        #[route("/series/:id")]
        Series { id: String },
        #[route("/reading")]
        Reading {},
        #[route("/watchlist")]
        Watchlist {},
        #[route("/notifications")]
        Notifications {},
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
    // App-wide contexts: the session (token + role) and the unread-notification badge.
    use_context_provider(Session::new);
    use_context_provider(|| UnreadBadge(Signal::new(0)));

    rsx! {
        document::Stylesheet { href: asset!("/assets/main.css") }
        Router::<Route> {}
    }
}
