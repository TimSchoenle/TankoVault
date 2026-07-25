//! `TankoVault` frontend — Dioxus (WASM SPA) + the Inkstone design system (design §17).
//!
//! A reader-facing SPA plus an operator console. Client state lives in Dioxus signals; server
//! state in `use_resource` fetch-caches over the **generated** API client; the access token
//! lives in memory only and is re-adopted from the httpOnly refresh cookie on boot. Routes are
//! type-safe via `dioxus-router`.
//!
//! Layout:
//! - [`app`] — the route table, root contexts and bundled font faces.
//! - [`api`] — a `Copy` handle that mints clients carrying the *current* bearer token.
//! - [`state`] — session, JWT claim decoding and appearance preferences.
//! - [`i18n`] — the message catalogues and the `Translator` handle every screen renders through.
//! - [`components`] — the shell, rail, command bar and the shared feedback primitives.
//! - [`views`] — the screens.
//! - [`hooks`] / [`util`] — refetch/busy handles and dependency-free formatting.

// Component functions are intentionally PascalCase. `#[component]` handles the lint locally,
// but the route table names them too, so the allow stays crate-wide for clarity.
#![allow(non_snake_case)]

mod api;
mod app;
mod components;
mod hooks;
mod i18n;
mod icons;
mod live;
mod models;
mod state;
mod util;
mod views;
mod wire;

pub(crate) use app::Route;

fn main() {
    dioxus::launch(app::App);
}
