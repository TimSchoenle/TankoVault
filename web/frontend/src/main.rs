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
//! - [`browser`] — the app's whole direct browser-API surface (storage, `<html>` attributes,
//!   navigation), typed through `web-sys` rather than `document::eval`.

// The crate-level `#![allow(non_snake_case)]` that used to sit here is **gone**, and the
// comment justifying it was wrong: it claimed the route table names components too, so the
// suppression had to be crate-wide. `#[component]` handles every one of them locally.
// Converting the `allow` to an `expect` (BUILD_AND_OPS §2.3) is what said so — a suppression
// that has stopped doing anything is silent as an `allow` and warns as an `expect`.

mod api;
mod app;
mod browser;
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
