//! `TankoVault` frontend — Dioxus + the Inkstone design system (design §17).
//!
//! One component tree, two builds: a WASM SPA served by the API (`--features web`, the default)
//! and a native desktop app in a wry webview (`--no-default-features --features desktop`).
//! Everything either one needs from the system it runs on is behind [`platform`]; nothing in
//! `views` knows which it is.
//!
//! A reader-facing SPA plus an operator console; the access token lives in memory only and is
//! re-adopted from the httpOnly refresh cookie on boot — on both builds.

mod api;
mod app;
mod build_info;
mod components;
mod hooks;
mod i18n;
mod icons;
mod live;
mod models;
mod platform;
mod state;
mod title;
mod util;
mod views;
mod webauthn;
mod wire;

pub(crate) use app::Route;

#[cfg(feature = "web")]
fn main() {
    dioxus::launch(app::App);
}

#[cfg(feature = "desktop")]
fn main() {
    use dioxus::desktop::{Config, LogicalSize, WindowBuilder};

    dioxus::LaunchBuilder::desktop()
        .with_cfg(
            Config::new()
                // `tao` installs a default "Window / Edit" menu bar when none is given. It is
                // the macOS convention leaking onto every platform, it acts on a text field
                // this app has none of at the top level, and it puts a strip of chrome above
                // the design system's own.
                .with_menu(None)
                .with_window(with_platform_chrome(
                    WindowBuilder::new()
                        // The app draws its own — see `components::TitleBar`, which also
                        // records what that costs (Windows 11's snap-layouts flyout).
                        .with_decorations(false)
                        // Names the taskbar button and the alt-tab entry; `crate::title`
                        // takes over from first render.
                        .with_title("TankoVault")
                        // Wide enough for the watchlist's full column set (the 1500px step
                        // adds two more), and a floor below the rail's own breakpoint so the
                        // responsive layout, not a resize limit, decides what is shown.
                        .with_inner_size(LogicalSize::new(1280.0, 860.0))
                        .with_min_inner_size(LogicalSize::new(480.0, 560.0)),
                )),
        )
        .launch(app::App);
}

/// A window with no decorations loses its drop shadow and its resize border along with them.
///
/// Windows can give both back while leaving the caption off, which is what makes an app-drawn
/// title bar look like a window rather than a rectangle painted on the desktop. Nothing to do on
/// the other platforms: the compositor keeps drawing the shadow there.
#[cfg(all(feature = "desktop", windows))]
fn with_platform_chrome(builder: dioxus::desktop::WindowBuilder) -> dioxus::desktop::WindowBuilder {
    use dioxus::desktop::tao::platform::windows::WindowBuilderExtWindows as _;
    builder.with_undecorated_shadow(true)
}

#[cfg(all(feature = "desktop", not(windows)))]
fn with_platform_chrome(builder: dioxus::desktop::WindowBuilder) -> dioxus::desktop::WindowBuilder {
    builder
}
