//! `TankoVault` frontend — Dioxus (WASM SPA) + the Inkstone design system (design §17).
//!
//! A reader-facing SPA plus an operator console; the access token lives in memory only and is
//! re-adopted from the httpOnly refresh cookie on boot.

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
mod webauthn;
mod wire;

pub(crate) use app::Route;

fn main() {
    dioxus::launch(app::App);
}
