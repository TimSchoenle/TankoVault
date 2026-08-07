//! The native implementation of [`crate::platform`], for the wry webview — `WebView2` on
//! Windows, `WebKitGTK` on Linux.
//!
//! Two things have no browser equivalent and are decided here.
//!
//! **There is no served origin.** The web SPA is delivered by the API it talks to, so
//! `location.origin` is the answer; a desktop binary is delivered by nobody and has to be told.
//! [`server_origin`] holds what the reader entered at first run, and an empty answer is what
//! makes [`crate::views::connect`] render instead of the router.
//!
//! **The document root is not reachable from Rust.** Dioxus can address the elements it renders
//! and nothing above them, and the one API that could reach `<html>` is `eval`, which this crate
//! bans (see `mod.rs`). Appearance attributes are therefore held in [`ROOT_ATTRIBUTES`] and
//! rendered onto the app's own root element by `crate::app::App`; the stylesheet already selects
//! on bare `[data-theme]`/`[data-accent]` attributes rather than `:root[…]`, so they cascade
//! from there unchanged.
//!
//! What is stored on disk is the server URL and the appearance preferences. **The access token
//! is not, and must not be** — it lives in memory for exactly as long as the process does, which
//! is the property the web build's CSP rules exist to protect. Having a filesystem is not a
//! reason to weaken it.

use dioxus::prelude::*;
use futures_util::StreamExt as _;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{OnceLock, RwLock};

/// Name of the settings document inside the platform's config directory.
const SETTINGS_FILE_NAME: &str = "settings.json";

/// The key [`server_origin`] is stored under. Namespaced like the appearance keys so one file
/// holds both.
const SERVER_ORIGIN_KEY: &str = "tv-server-origin";

/// Whether a push raises an OS notification. Absent means on.
const DESKTOP_NOTIFICATIONS_KEY: &str = "tv-desktop-notifications";

// ---------------------------------------------------------------------------------------------
// Settings file
// ---------------------------------------------------------------------------------------------

/// The parsed settings document, plus where it came from.
///
/// `path` is `None` when the platform exposes no config directory. Every read then answers
/// `None` and every write is dropped, which is the same contract the web side has under blocked
/// storage: the caller's default is a correct answer.
struct Settings {
    path: Option<PathBuf>,
    values: BTreeMap<String, String>,
}

fn settings() -> &'static RwLock<Settings> {
    static SETTINGS: OnceLock<RwLock<Settings>> = OnceLock::new();
    SETTINGS.get_or_init(|| RwLock::new(Settings::load()))
}

impl Settings {
    /// Read the settings document once, tolerating every way it can be absent or unreadable.
    fn load() -> Self {
        let path = directories::ProjectDirs::from("dev", "", "TankoVault")
            .map(|dirs| dirs.config_dir().join(SETTINGS_FILE_NAME));
        let values = path
            .as_ref()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| serde_json::from_str::<BTreeMap<String, String>>(&text).ok())
            .unwrap_or_default();
        Self { path, values }
    }

    /// Write the document out through a temporary file and a rename.
    ///
    /// Not a plain truncate-and-write: a crash or a full disk partway through that leaves a
    /// half-written JSON document, which the next start silently discards — taking the reader's
    /// configured server with it and dropping them back on the first-run screen.
    fn flush(&self) {
        let Some(path) = self.path.as_ref() else {
            return;
        };
        let Some(dir) = path.parent() else {
            return;
        };
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
        let Ok(text) = serde_json::to_string_pretty(&self.values) else {
            return;
        };
        let staged = path.with_extension("json.tmp");
        if std::fs::write(&staged, text).is_ok() {
            let _ = std::fs::rename(&staged, path);
        }
    }
}

/// Where the settings document lives, shown on the connection screen so an operator can find —
/// or delete — the one thing this app writes outside its install directory.
pub(crate) fn settings_path() -> Option<PathBuf> {
    settings().read().ok()?.path.clone()
}

pub(crate) fn store_get(key: &str) -> Option<String> {
    settings().read().ok()?.values.get(key).cloned()
}

pub(crate) fn store_set(key: &str, value: &str) {
    let Ok(mut settings) = settings().write() else {
        return;
    };
    settings.values.insert(key.to_owned(), value.to_owned());
    settings.flush();
}

pub(crate) fn store_remove(key: &str) {
    let Ok(mut settings) = settings().write() else {
        return;
    };
    settings.values.remove(key);
    settings.flush();
}

// ---------------------------------------------------------------------------------------------
// Server origin
// ---------------------------------------------------------------------------------------------

/// The server the reader connected to, or `None` before first run.
pub(crate) fn server_origin() -> Option<String> {
    store_get(SERVER_ORIGIN_KEY).filter(|origin| !origin.is_empty())
}

/// Remember `origin` as the server to talk to. `None` forgets it, which returns the app to the
/// first-run screen on the next start.
pub(crate) fn set_server_origin(origin: Option<&str>) {
    match origin {
        Some(origin) => store_set(SERVER_ORIGIN_KEY, origin.trim_end_matches('/')),
        None => store_remove(SERVER_ORIGIN_KEY),
    }
}

pub(crate) fn origin() -> String {
    server_origin().unwrap_or_default()
}

// ---------------------------------------------------------------------------------------------
// The window
// ---------------------------------------------------------------------------------------------

/// The OS window, or `None` outside the Dioxus runtime.
///
/// `try_consume_context`, never `dioxus::desktop::window()`: that one panics when there is no
/// desktop context, and a panic aborts the process (`panic = "abort"`) — over a title bar button.
pub(crate) fn window() -> Option<dioxus::desktop::DesktopContext> {
    try_consume_context::<dioxus::desktop::DesktopContext>()
}

/// Shrink and re-centre the window if the size it was built with does not fit the display.
///
/// `WindowBuilder` picks a size before there is an event loop, so it cannot know which monitor
/// the window will open on. This runs once from the first render, when it can.
///
/// 92% of the monitor, not 100%: `tao` reports the monitor, not its *work area*, so the taskbar
/// or dock is included in that number and a window sized to it would sit under one edge.
/// Growing is never attempted — a reader who wants it bigger has a maximise button, and a window
/// that inflates itself on launch is worse than one that is merely smaller than the screen.
pub(crate) fn fit_window_to_display() {
    const MAX_FRACTION: f64 = 0.92;

    let Some(window) = window() else {
        return;
    };
    let Some(monitor) = window.current_monitor() else {
        return;
    };

    let scale = monitor.scale_factor();
    let available = monitor.size().to_logical::<f64>(scale);
    let current = window.inner_size().to_logical::<f64>(scale);

    let width = current.width.min(available.width * MAX_FRACTION);
    let height = current.height.min(available.height * MAX_FRACTION);
    if width >= current.width && height >= current.height {
        return;
    }

    window.set_inner_size(dioxus::desktop::LogicalSize::new(width, height));
    // Re-centred as well: shrinking from the top-left corner alone leaves the window wherever
    // the OS first placed a box of the old size, which after a resize is rarely centred.
    let position = monitor.position().to_logical::<f64>(scale);
    window.set_outer_position(dioxus::desktop::LogicalPosition::new(
        position.x + (available.width - width) / 2.0,
        position.y + (available.height - height) / 2.0,
    ));
}

// ---------------------------------------------------------------------------------------------
// OS notifications
// ---------------------------------------------------------------------------------------------

/// Whether a push should raise an OS notification. Defaults to on: the app exists to tell you a
/// chapter landed, and a reader who does not want that has a switch (below) rather than a
/// default that hides it.
pub(crate) fn notifications_enabled() -> bool {
    store_get(DESKTOP_NOTIFICATIONS_KEY).is_none_or(|stored| stored != "0")
}

pub(crate) fn set_notifications_enabled(enabled: bool) {
    if enabled {
        store_remove(DESKTOP_NOTIFICATIONS_KEY);
    } else {
        store_set(DESKTOP_NOTIFICATIONS_KEY, "0");
    }
}

/// Raise an OS notification.
///
/// Best-effort and silent on failure, which is the same contract the rest of this module has: a
/// desktop with no notification daemon, a Windows install that has muted the app, or a
/// focus-assist session are all "the reader did not want this", not something to interrupt them
/// about. The in-app badge is updated either way and is the source of truth.
///
/// Runs on its own thread. `notify-rust` talks D-Bus on Linux and `WinRT` on Windows, and
/// neither is something to block a UI frame on.
pub(crate) fn notify(summary: &str, body: &str) {
    let (summary, body) = (summary.to_owned(), body.to_owned());
    std::thread::spawn(move || {
        let _ = notify_rust::Notification::new()
            .summary(&summary)
            .body(&body)
            // Matches the bundle identifier, which is what a desktop environment keys an app's
            // notification settings and icon off.
            .appname("TankoVault")
            .show();
    });
}

// ---------------------------------------------------------------------------------------------
// Appearance attributes
// ---------------------------------------------------------------------------------------------

/// The `data-*` attributes the app's root element carries. See the module contract for why they
/// cannot live on `<html>` here.
#[derive(Clone, Default, PartialEq, Eq)]
pub(crate) struct AppearanceAttributes(BTreeMap<String, String>);

impl AppearanceAttributes {
    /// The value of `name`, or `None` so the renderer omits the attribute entirely — an empty
    /// `data-theme=""` would match none of the stylesheet's rules but is still noise in the tree.
    pub(crate) fn get(&self, name: &str) -> Option<String> {
        self.0.get(name).cloned()
    }
}

/// Read by `crate::app::App`, which renders these onto the element every screen sits inside.
pub(crate) static ROOT_ATTRIBUTES: GlobalSignal<AppearanceAttributes> =
    Signal::global(AppearanceAttributes::default);

pub(crate) fn root_attribute(name: &str) -> Option<String> {
    ROOT_ATTRIBUTES.read().get(name)
}

pub(crate) fn set_root_attribute(name: &str, value: &str) {
    ROOT_ATTRIBUTES
        .write()
        .0
        .insert(name.to_owned(), value.to_owned());
}

pub(crate) fn remove_root_attribute(name: &str) {
    ROOT_ATTRIBUTES.write().0.remove(name);
}

pub(crate) fn set_document_language(tag: &str) {
    set_root_attribute("lang", tag);
}

/// The screen's own name, for the app-drawn title bar to render.
///
/// Deliberately *not* the window title. The OS title is decorated for the taskbar and alt-tab
/// (`"Home — TankoVault"`), and repeating the brand in a header that sits directly above the
/// rail's own wordmark says nothing twice. The bar also renders above the router, so it cannot
/// ask what the route is called — see [`crate::components::TitleBar`]. Empty until the first
/// routed screen mounts, which is the connection screen, and the bar falls back to the product
/// name there.
pub(crate) static WINDOW_HEADING: GlobalSignal<String> = Signal::global(String::new);

/// The OS window's title, which is this platform's answer to `document.title`.
///
/// `try_consume_context`, not `dioxus::desktop::window()`: that one panics when there is no
/// desktop context, and a panic here aborts the process (`panic = "abort"`) over a title.
pub(crate) fn set_document_title(title: &str) {
    if let Some(window) = try_consume_context::<dioxus::desktop::DesktopContext>() {
        window.set_title(title);
    }
}

/// Publish the undecorated screen name for the title bar.
///
/// Guarded: `set` invalidates unconditionally, and this runs from an effect that re-fires on
/// every render of the routed screen.
pub(crate) fn set_window_heading(heading: &str) {
    if *WINDOW_HEADING.peek() != heading {
        heading.clone_into(&mut WINDOW_HEADING.write());
    }
}

/// Hands `url` to the user's own browser.
///
/// The destinations this is used for are OAuth consent screens, which belong in the browser
/// where the reader is already signed in to that provider — and which a webview would in any
/// case be refused by several of them.
pub(crate) fn navigate_to(url: &str) {
    let _ = open::that_detached(url);
}

/// A no-op: `MountedData` has no selection API, and the only way to reach the DOM's would be
/// `eval`, which this crate bans. The field is focused either way — see the surface's contract.
pub(crate) fn select_focused_text() {}

// ---------------------------------------------------------------------------------------------
// Clock
// ---------------------------------------------------------------------------------------------

#[expect(
    clippy::cast_precision_loss,
    reason = "f64 is exact to 2^53 ms — year 287396 — and the surface is f64 because the browser \
              clock this mirrors is"
)]
pub(crate) fn now_ms() -> f64 {
    chrono::Utc::now().timestamp_millis() as f64
}

/// The API emits RFC-3339 and nothing else, so this parses that and reports `NAN` for anything
/// it does not recognise — matching `Date.parse`, whose callers render the raw string on `NAN`.
#[expect(
    clippy::cast_precision_loss,
    reason = "see `now_ms`; the same millisecond range, the same exactness"
)]
pub(crate) fn parse_timestamp_ms(text: &str) -> f64 {
    chrono::DateTime::parse_from_rfc3339(text)
        .map_or(f64::NAN, |parsed| parsed.timestamp_millis() as f64)
}

/// Millisecond precision and a `Z` suffix, matching what `Date.prototype.toISOString` emits on
/// the other side — the API parses both, but a parameter that differs by build is a difference
/// waiting to be depended on.
#[expect(
    clippy::cast_possible_truncation,
    reason = "the input is an epoch-millisecond value this crate computed from `now_ms`; \
              anything outside i64 is not an instant and falls out as `None` below"
)]
pub(crate) fn format_timestamp_iso(ms: f64) -> Option<String> {
    chrono::DateTime::from_timestamp_millis(ms as i64)
        .map(|at| at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
}

pub(crate) fn local_hour() -> u32 {
    use chrono::Timelike as _;
    chrono::Local::now().hour()
}

pub(crate) async fn sleep_ms(ms: u32) {
    tokio::time::sleep(std::time::Duration::from_millis(u64::from(ms))).await;
}

pub(crate) fn preferred_language() -> Option<String> {
    sys_locale::get_locale()
}

// ---------------------------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------------------------

/// Ask for a destination and write `contents` there.
///
/// Cancelling the dialog is reported as its own key rather than as success: the caller renders
/// "export saved" on `Ok`, and saying that about a file nobody chose a home for would be a lie.
pub(crate) async fn save_text_file(
    filename: &str,
    _mime: &str,
    contents: &str,
) -> Result<(), &'static str> {
    let Some(handle) = rfd::AsyncFileDialog::new()
        .set_file_name(filename)
        .save_file()
        .await
    else {
        return Err("common.downloadCancelled");
    };
    handle
        .write(contents.as_bytes())
        .await
        .map_err(|_| "common.downloadRefused")
}

// ---------------------------------------------------------------------------------------------
// Server-sent events
// ---------------------------------------------------------------------------------------------

/// A held-open `text/event-stream` response, parsed into events.
pub(crate) struct EventStream {
    events: std::pin::Pin<Box<dyn futures_util::Stream<Item = SseItem>>>,
    /// Only messages carrying one of these `event:` names are delivered, which is what the
    /// browser's per-name `EventSource` subscriptions do on the other side.
    wanted: Vec<String>,
}

type SseItem =
    Result<eventsource_stream::Event, eventsource_stream::EventStreamError<reqwest::Error>>;

impl EventStream {
    pub(crate) async fn next(&mut self) -> Option<(String, String)> {
        loop {
            match self.events.next().await? {
                Ok(event) if self.wanted.contains(&event.event) => {
                    return Some((event.event, event.data));
                }
                Ok(_) => {}
                // The ticket in the URL is spent, so no retry from here could succeed; end the
                // attempt and let the caller mint a fresh one.
                Err(_) => return None,
            }
        }
    }

    /// Dropping the response closes the connection; this exists so both platforms end a stream
    /// the same way.
    pub(crate) fn close(self) {
        drop(self);
    }
}

pub(crate) async fn subscribe(url: &str, events: &[&str]) -> Option<EventStream> {
    use eventsource_stream::Eventsource as _;

    let response = reqwest::Client::new()
        .get(url)
        .header(reqwest::header::ACCEPT, "text/event-stream")
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    Some(EventStream {
        events: Box::pin(response.bytes_stream().eventsource()),
        wanted: events.iter().map(|name| (*name).to_owned()).collect(),
    })
}
