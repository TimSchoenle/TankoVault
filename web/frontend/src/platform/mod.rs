//! Everything this app needs from the system it runs on, behind one surface with two
//! implementations: `web` (WASM in a browser) and `desktop` (a wry webview — `WebView2` on
//! Windows, `WebKitGTK` on Linux). Exactly one is compiled.
//!
//! Every function below is declared here and delegated to the active implementation, so the
//! contract is written once and both sides are held to the same one. Views never learn which
//! they are on.
//!
//! This is also what keeps the Content-Security-Policy contract checkable. The web side is the
//! only place in the crate that touches a browser API, and it does so through `web-sys` rather
//! than `document::eval`: Dioxus implements web `eval` as `new Function(code)`, a CSP without
//! `'unsafe-eval'` blocks it, and the failure is not caught — it aborts the WASM instance. This
//! app hit exactly that on boot, reading appearance prefs: white page, dead instance. The
//! desktop webview is served under the same CSP for the same reason, so neither side may reach
//! for eval to fill a gap in this surface. Add to the surface instead.
//!
//! Storage can fail silently (private mode, blocked third-party storage, an unwritable config
//! directory); every caller's fallback — a boot-script attribute or the stylesheet default — is
//! a correct answer, not a reason to interrupt the reader.

#[cfg(all(feature = "web", feature = "desktop"))]
compile_error!(
    "features `web` and `desktop` are mutually exclusive — they select different \
     implementations of `crate::platform` and different Dioxus renderers. Build the desktop \
     app with `--no-default-features --features desktop`."
);
#[cfg(not(any(feature = "web", feature = "desktop")))]
compile_error!("exactly one of the `web` or `desktop` features must be enabled");

#[cfg(feature = "desktop")]
mod desktop;
#[cfg(feature = "web")]
mod web;

#[cfg(feature = "desktop")]
use desktop as imp;
#[cfg(feature = "web")]
use web as imp;

/// A live server-sent-event subscription. See [`subscribe`].
pub(crate) use imp::EventStream;

/// Desktop-only surface: there is no browser equivalent to reach for, so these have no
/// counterpart on the other side and their callers are `#[cfg]`-gated too.
///
/// The window controls exist because the desktop build draws its own title bar — the OS caption
/// is switched off in `main`, so minimising, maximising, moving and closing the window are the
/// app's job now. See [`crate::components::TitleBar`].
///
/// `autostart_*` is the reader's sign-in list — an `HKCU\…\Run` value on Windows, a freedesktop
/// `autostart` entry on Linux. There is no browser equivalent and there should not be: a web page
/// cannot arrange to be opened at login, and an installed app is the only thing entitled to ask.
///
/// `credential_*` is the OS credential store — Credential Manager, Secret Service, Keychain.
/// The web build has no counterpart because it needs none: the browser already keeps the refresh
/// cookie encrypted, scoped to the origin and out of reach of script. On desktop that guarantee
/// has to be asked for, and the credential store is the only thing that offers it; a file beside
/// `settings.json` would be a bearer credential readable by every process running as the reader.
/// Never for the access token, which stays in memory on both sides. See
/// [`crate::api::session_store`].
#[cfg(feature = "desktop")]
pub(crate) use desktop::{
    autostart_enabled, autostart_supported, credential_delete, credential_get, credential_set,
    fit_window_to_display, notifications_enabled, notify, server_origin, set_autostart,
    set_notifications_enabled, set_server_origin, set_window_heading, settings_path, window,
    ROOT_ATTRIBUTES, STARTUP_INNER_SIZE, WINDOW_HEADING,
};

// ---------------------------------------------------------------------------------------------
// Persistent key/value settings
// ---------------------------------------------------------------------------------------------

/// Read `key` from persistent settings, or `None` if unset or the store is unavailable.
pub(crate) fn store_get(key: &str) -> Option<String> {
    imp::store_get(key)
}

/// Persist `value` under `key`. A refusal is silent; see the module contract.
pub(crate) fn store_set(key: &str, value: &str) {
    imp::store_set(key, value);
}

/// Forget `key`, so whatever default the caller falls back to takes over again.
pub(crate) fn store_remove(key: &str) {
    imp::store_remove(key);
}

// ---------------------------------------------------------------------------------------------
// Appearance attributes
// ---------------------------------------------------------------------------------------------

/// Read an appearance attribute (`data-theme`, `data-accent`, …).
///
/// On web this is the attribute the boot script in `index.html` applied before first paint,
/// which is the fallback when storage has no answer. Desktop has no pre-paint script — it reads
/// the settings file synchronously at startup instead — so there is nothing earlier to fall back
/// to and this answers from what the app has already applied.
pub(crate) fn root_attribute(name: &str) -> Option<String> {
    imp::root_attribute(name)
}

/// Apply an appearance attribute, which is what the `[data-*]` rules in the stylesheet select on.
pub(crate) fn set_root_attribute(name: &str, value: &str) {
    imp::set_root_attribute(name, value);
}

/// Drop an appearance attribute, handing the choice back to the `:root` defaults.
pub(crate) fn remove_root_attribute(name: &str) {
    imp::remove_root_attribute(name);
}

/// Mirror the active language onto the document, for screen-reader voice selection,
/// hyphenation and locale-aware font fallback.
pub(crate) fn set_document_language(tag: &str) {
    imp::set_document_language(tag);
}

/// Name the browser tab — or the OS window — after the screen on display (see [`crate::title`]).
///
/// Not `document::Title`: `dioxus-web` implements it as `eval("document.title = …")`, which is
/// the call the served CSP aborts the WASM instance over.
pub(crate) fn set_document_title(title: &str) {
    imp::set_document_title(title);
}

/// Leave the app for `url` — a real navigation on web, the user's own browser on desktop.
///
/// For destinations outside this origin (an OAuth consent screen), never for in-app routing.
pub(crate) fn navigate_to(url: &str) {
    imp::navigate_to(url);
}

/// Select the contents of the focused text field, so the next keystroke replaces them.
///
/// Focusing itself is the renderer's job (`MountedData::set_focus`, see
/// [`crate::components::focus_and_select`]); this is only the selection, and it is a nicety, not
/// a behaviour anything depends on. The desktop renderer exposes no equivalent and this is a
/// no-op there — the field is still focused, it just keeps its caret at the end.
pub(crate) fn select_focused_text() {
    imp::select_focused_text();
}

// ---------------------------------------------------------------------------------------------
// Clock
// ---------------------------------------------------------------------------------------------

/// Milliseconds since the Unix epoch.
pub(crate) fn now_ms() -> f64 {
    imp::now_ms()
}

/// Parse an RFC-3339 timestamp to milliseconds since the epoch, or `f64::NAN` if it cannot be
/// read. Callers render the raw string on `NAN` rather than inventing an age.
pub(crate) fn parse_timestamp_ms(text: &str) -> f64 {
    imp::parse_timestamp_ms(text)
}

/// Render epoch milliseconds as the UTC RFC-3339 string the API's date parameters take, or
/// `None` if the value is not a representable instant.
pub(crate) fn format_timestamp_iso(ms: f64) -> Option<String> {
    imp::format_timestamp_iso(ms)
}

/// The hour of the day in the reader's own timezone, `0..=23`.
pub(crate) fn local_hour() -> u32 {
    imp::local_hour()
}

/// Yield for `ms`.
///
/// Never `std::thread::sleep`, on either platform: it freezes the browser's main thread on web
/// and blocks a runtime worker on desktop. Banned in `clippy.toml`.
pub(crate) async fn sleep_ms(ms: u32) {
    imp::sleep_ms(ms).await;
}

// ---------------------------------------------------------------------------------------------
// Environment
// ---------------------------------------------------------------------------------------------

/// The reader's preferred language tag (`en-GB`), if the system exposes one.
pub(crate) fn preferred_language() -> Option<String> {
    imp::preferred_language()
}

/// Absolute base URL for API calls (design §19).
///
/// On web this is the origin the SPA was served from, which is the API's own. Desktop has no
/// served origin, so it is the server the reader connected to at first run; empty until then,
/// which is the state [`crate::views::connect`] exists to resolve.
pub(crate) fn origin() -> String {
    imp::origin()
}

/// Hand `contents` to the system as a document named `filename`.
///
/// The one caller shape is a bearer-authenticated export the app already holds in memory:
/// pointing a link at the endpoint cannot work, because a plain navigation carries no
/// `Authorization` header.
///
/// # Errors
/// A **catalogue key**, not a sentence — resolved through the caller's translator.
pub(crate) async fn save_text_file(
    filename: &str,
    mime: &str,
    contents: &str,
) -> Result<(), &'static str> {
    imp::save_text_file(filename, mime, contents).await
}

// ---------------------------------------------------------------------------------------------
// Server-sent events
// ---------------------------------------------------------------------------------------------

/// Open an SSE subscription to `url`, delivering `(event name, data)` for every message whose
/// `event:` is one of `events`.
///
/// The name is carried alongside the payload because one stream can serve several: the console
/// takes `stats` and `runs` off a single connection, on their own cadences.
///
/// `url` carries a single-use ticket rather than the access token — see
/// [`crate::api::stream_url`] — so both implementations authenticate the same way and neither
/// needs to set a header. Returns `None` if the stream cannot be opened at all; the caller
/// treats that as an attempt that failed and backs off.
pub(crate) async fn subscribe(url: &str, events: &[&str]) -> Option<EventStream> {
    imp::subscribe(url, events).await
}
