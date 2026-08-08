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
//! What the settings file holds is the server URL and the appearance preferences, and nothing
//! else. **The access token is not in it, and must not be** — it lives in memory for exactly as
//! long as the process does, which is the property the web build's CSP rules exist to protect.
//! Having a filesystem is not a reason to weaken it.
//!
//! The refresh credential *is* kept between runs, but never as a file: it goes to the OS
//! credential store through [`credential_get`] and friends, which is the one place on a desktop
//! that offers what the browser's cookie jar offers on the other side — encryption at rest under
//! the user's login, and access scoped to their account rather than to anything that can read
//! their config directory. See `crate::api::session_store` for what is written there.

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
/// or delete — the one *file* this app writes outside its install directory. The other thing it
/// writes is the refresh credential, which is not a file and is not here; it is an entry in the
/// OS credential store, and signing out removes it (see [`credential_delete`]).
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
// OS credential store
// ---------------------------------------------------------------------------------------------

/// The service name every entry is filed under — what the reader sees next to the secret in the
/// Windows Credential Manager or Seahorse, and what they delete to sign the app out from outside
/// it.
const KEYRING_SERVICE: &str = "TankoVault";

/// One unit of work for the credential thread. `Get` carries its own reply channel because the
/// caller has to wait for the answer; the other two are fire-and-forget.
enum CredentialRequest {
    Get(String, std::sync::mpsc::Sender<Option<String>>),
    Set(String, String),
    Delete(String),
}

/// The one thread every credential-store call is funnelled through. **Do not inline these calls
/// at the call site.** Two separate things depend on this indirection.
///
/// **On Linux it is what stops the process aborting.** The Secret Service backend is
/// `secret-service`'s blocking API, which is `zbus::blocking`, which is
/// `tokio::runtime::Runtime::block_on` — because `notify-rust`'s `z-with-tokio` turns on zbus's
/// `tokio` feature for the whole graph. `block_on` panics when the calling thread is already
/// inside a runtime ("Cannot start a runtime from within a runtime"), and [`credential_set`] is
/// called from `reqwest`'s response path, which *is* a runtime worker. This crate builds with
/// `panic = "abort"`, so that panic is not an error anyone sees — it is the app disappearing
/// while the reader signs in. A plain OS thread carries no runtime context, so the call is legal
/// here and nowhere else.
///
/// **And it is the ordering guarantee.** One FIFO, not a thread per call: two independently
/// spawned threads could land a stale write *after* a sign-out's delete and leave a revoked
/// credential on disk. Reads go through it for the same reason rather than around it.
fn credentials() -> &'static std::sync::mpsc::Sender<CredentialRequest> {
    static REQUESTS: OnceLock<std::sync::mpsc::Sender<CredentialRequest>> = OnceLock::new();
    REQUESTS.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<CredentialRequest>();
        std::thread::spawn(move || {
            while let Ok(request) = rx.recv() {
                match request {
                    CredentialRequest::Get(account, reply) => {
                        let secret = entry(&account).and_then(|e| e.get_password().ok());
                        let _ = reply.send(secret);
                    }
                    CredentialRequest::Set(account, secret) => {
                        if let Some(entry) = entry(&account) {
                            let _ = entry.set_password(&secret);
                        }
                    }
                    CredentialRequest::Delete(account) => {
                        if let Some(entry) = entry(&account) {
                            let _ = entry.delete_credential();
                        }
                    }
                }
            }
        });
        tx
    })
}

/// An entry handle, or `None` when the platform has no usable credential store — a headless
/// Linux session with no Secret Service provider is the ordinary case. Absent is a valid answer
/// here for the same reason it is for the settings file: the caller's fallback is "signed out",
/// not an error to interrupt the reader with.
fn entry(account: &str) -> Option<keyring::Entry> {
    keyring::Entry::new(KEYRING_SERVICE, account).ok()
}

/// The secret stored under `account`, or `None` if there is none or the store is unavailable.
///
/// Blocks until the credential thread answers. Callers are boot-time only, by design — see
/// [`credentials`].
pub(crate) fn credential_get(account: &str) -> Option<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    credentials()
        .send(CredentialRequest::Get(account.to_owned(), tx))
        .ok()?;
    rx.recv().ok().flatten()
}

/// Store `secret` under `account`, replacing whatever was there. Queued; a refusal is silent.
pub(crate) fn credential_set(account: &str, secret: &str) {
    let _ = credentials().send(CredentialRequest::Set(
        account.to_owned(),
        secret.to_owned(),
    ));
}

/// Forget `account` entirely. Queued behind any pending write for that account, which is what
/// makes a sign-out final rather than a race.
pub(crate) fn credential_delete(account: &str) {
    let _ = credentials().send(CredentialRequest::Delete(account.to_owned()));
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

/// The inner size the window opens at, before [`fit_window_to_display`] replaces it.
///
/// Declared here rather than left at `main`'s `WindowBuilder` because the fit below is defined
/// against it: this is a placeholder, and the contract is that no screen is ever *laid out* at it.
pub(crate) const STARTUP_INNER_SIZE: (f64, f64) = (1280.0, 860.0);

/// Of the monitor's **work area**, in both directions.
///
/// Breathing room and nothing else now: the taskbar, dock or panel is subtracted by
/// [`work_area`] before this applies, so the window no longer has to be shrunk on the chance that
/// something is parked along an edge. That is what it used to be for — `0.82` of the whole
/// monitor was one number doing two jobs, and it did the second one by guessing. Raised to `0.9`
/// because it is only doing the first one now; a window that still leaves a visible frame of
/// desktop on each side reads as a window rather than a botched maximise.
///
/// (A third constant, `MAX_FRACTION: 0.92`, used to sit here claiming the taskbar job. It never
/// bound at any width, because `0.82 * w` is the smaller of the two everywhere.)
const PREFERRED_FRACTION: f64 = 0.9;

/// What the window carries besides the content column, and what the ceiling below is built from.
///
/// These mirror `--rail-w` and `--gutter` in `input.css` and the widest `--measure` any route
/// asks for in `crate::components::shell`. Nothing compiles a Rust constant against a stylesheet,
/// so `xtask repo-lint`'s `window-ceiling-matches-the-layout` reconciles them.
const RAIL_WIDTH: f64 = 280.0;
/// Padding either side of the content band. The `:root` value: the narrow-viewport override is
/// far below any width this ceiling is about.
const CONTENT_GUTTER: f64 = 40.0;
/// `.ik-desktop-body` scrolls rather than the window, and its scrollbar takes layout width.
const SCROLLBAR_WIDTH: f64 = 15.0;
/// The widest measured content column any route asks for.
const WIDEST_MEASURE: f64 = 1760.0;

/// The widest window the layout can still fill.
///
/// Past this, a wider window buys margin rather than covers: every band inside `.ik-main` is
/// capped at `--measure`, so the extra pixels land in the gutters either side of a column that
/// has stopped growing. It also stops a window spanning an ultrawide, where that capped column
/// would sit in a narrow strip down the middle of a very wide frame.
///
/// A sum rather than a number because the number was wrong. `1760` was hardcoded here — but that
/// is the cap on the *content column*, and the window also carries the rail, both gutters and the
/// body's scrollbar. A 1760px window therefore left Discover a 1063px results column: five covers
/// across, where the same layout fits seven once the window is wide enough to reach the cap. A
/// ceiling stated in the units of the thing it bounds cannot drift from it that way.
fn widest_useful_window() -> f64 {
    RAIL_WIDTH + 2.0 * CONTENT_GUTTER + SCROLLBAR_WIDTH + WIDEST_MEASURE
}

/// Longest [`fit_window_to_display`] waits for the window to report its new size before letting
/// the app render anyway. A backstop, not a delay — the ordinary case resolves in a frame or two.
const FIT_TIMEOUT_MS: u32 = 600;
/// Gap between size checks while waiting.
const FIT_POLL_MS: u32 = 16;

/// A monitor's usable rectangle, in logical pixels and absolute (virtual-screen) coordinates.
///
/// Absolute because the window is centred in it: a taskbar along the bottom moves the usable
/// area's *edge*, and a window centred on the monitor instead would sit low in what is left of
/// it — or under the taskbar.
#[derive(Clone, Copy)]
struct WorkArea {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

impl WorkArea {
    /// Reject a monitor that reports nothing usable — a display disconnected mid-query, a
    /// compositor still starting up — so the caller falls back instead of sizing to nothing.
    fn usable(self) -> Option<Self> {
        (self.width > 0.0 && self.height > 0.0).then_some(self)
    }
}

/// What the system leaves after its own furniture: the Windows taskbar, a GNOME panel, a dock.
///
/// `tao` reports the whole monitor and nothing else — `MonitorHandle` offers `size`, `position`
/// and `scale_factor` — so this asks the platform directly. Both implementations identify the
/// monitor by the rectangle `tao` already reports rather than by a handle borrowed across crates,
/// and both calls are safe: this crate `forbid`s unsafe, which is why `winsafe` is a dependency
/// and `windows` is not.
///
/// `None` means the platform would not say, and the caller falls back to the whole monitor. That
/// is a worse answer rather than a wrong one — it is the behaviour the fraction above used to be
/// shrunk to compensate for.
#[cfg(windows)]
fn work_area(monitor: &dioxus::desktop::tao::monitor::MonitorHandle) -> Option<WorkArea> {
    let position = monitor.position();
    let size = monitor.size();
    let bounds = winsafe::RECT {
        left: position.x,
        top: position.y,
        right: position.x.checked_add(i32::try_from(size.width).ok()?)?,
        bottom: position.y.checked_add(i32::try_from(size.height).ok()?)?,
    };
    let info = winsafe::HMONITOR::MonitorFromRect(bounds, winsafe::co::MONITOR::DEFAULTTONEAREST)
        .GetMonitorInfo()
        .ok()?;
    // `rcWork` is in physical pixels on the virtual screen; this module works in logical ones.
    let scale = monitor.scale_factor();
    WorkArea {
        x: f64::from(info.rcWork.left) / scale,
        y: f64::from(info.rcWork.top) / scale,
        width: f64::from(info.rcWork.right - info.rcWork.left) / scale,
        height: f64::from(info.rcWork.bottom - info.rcWork.top) / scale,
    }
    .usable()
}

/// See the Windows twin above. GDK already answers in application pixels — the logical ones this
/// module works in — so unlike `rcWork` there is no scale factor to divide out.
#[cfg(all(unix, not(target_vendor = "apple")))]
fn work_area(monitor: &dioxus::desktop::tao::monitor::MonitorHandle) -> Option<WorkArea> {
    use dioxus::desktop::tao::platform::unix::MonitorHandleExtUnix as _;
    use gdk::prelude::MonitorExt as _;

    let area = monitor.gdk_monitor().workarea();
    WorkArea {
        x: f64::from(area.x()),
        y: f64::from(area.y()),
        width: f64::from(area.width()),
        height: f64::from(area.height()),
    }
    .usable()
}

/// macOS is not shipped — the release workflow builds Windows and Linux — so this exists to keep
/// the module compiling on a developer's Mac rather than to serve anyone.
#[cfg(not(any(windows, all(unix, not(target_vendor = "apple")))))]
fn work_area(_monitor: &dioxus::desktop::tao::monitor::MonitorHandle) -> Option<WorkArea> {
    None
}

/// The inner size to open at within `available` logical pixels of usable area, or `None` when
/// nothing usable was reported.
///
/// Proportional rather than fixed, in both directions. A fixed default is either cramped on a
/// large display or taller than the screen on a laptop, and this app's densest screen — the
/// watchlist — reveals two more columns at 1500px, so the space is worth taking when it exists.
///
/// Only the width is capped, by [`widest_useful_window`]. Height is not: nothing in the layout
/// caps a page's height, so a taller window is always more rows, and the display is the only
/// limit worth having. It used to be capped at a flat 1120px, which cost a 1440p display 60px of
/// covers and a 4K one 650px.
fn fitted_inner_size(available: (f64, f64)) -> Option<(f64, f64)> {
    let (width, height) = available;
    if width <= 0.0 || height <= 0.0 {
        return None;
    }
    Some((
        (width * PREFERRED_FRACTION).min(widest_useful_window()),
        height * PREFERRED_FRACTION,
    ))
}

/// Size the window to the display it opened on, centre it, and resolve once the window reports
/// the new size.
///
/// `WindowBuilder` has to pick a size before there is an event loop, so it cannot know which
/// monitor the window will land on — [`STARTUP_INNER_SIZE`] is that placeholder, and this runs
/// once from the first render, when the monitor is knowable.
///
/// **Awaiting it is load-bearing, not tidiness.** [`crate::app::AppRoot`] holds the UI back until
/// this resolves, because a screen that mounts while the window is still the placeholder measures
/// the placeholder's geometry — and [`crate::components::use_grid_fit`] turns exactly that
/// measurement into a page size. Discover fetched a 1280px window's worth of covers, the resize
/// widened the grid under them, and the correction never landed: every page was laid out short,
/// leaving a ragged row and a band of dead space at each page boundary.
///
/// `window` is passed in rather than read here because this runs from a spawned task; the caller
/// takes the context during its own render, where it is reliably reachable.
pub(crate) async fn fit_window_to_display(window: Option<dioxus::desktop::DesktopContext>) {
    let Some(window) = window else {
        return;
    };
    let Some(monitor) = window.current_monitor() else {
        return;
    };

    let scale = monitor.scale_factor();
    let area = work_area(&monitor).unwrap_or_else(|| {
        let position = monitor.position().to_logical::<f64>(scale);
        let size = monitor.size().to_logical::<f64>(scale);
        WorkArea {
            x: position.x,
            y: position.y,
            width: size.width,
            height: size.height,
        }
    });
    let Some((width, height)) = fitted_inner_size((area.width, area.height)) else {
        return;
    };

    window.set_inner_size(dioxus::desktop::LogicalSize::new(width, height));
    // Centred as well: resizing moves the bottom-right corner only, so a window the OS placed
    // for the old size ends up off-centre — and, when it grew, possibly off-screen. Centred in
    // the *work area*, so a taskbar along one edge shifts the window off it rather than under it.
    window.set_outer_position(dioxus::desktop::LogicalPosition::new(
        area.x + (area.width - width) / 2.0,
        area.y + (area.height - height) / 2.0,
    ));

    for _ in 0..FIT_TIMEOUT_MS.div_ceil(FIT_POLL_MS) {
        let now = window.inner_size().to_logical::<f64>(window.scale_factor());
        // A pixel of slack: the OS answers in physical pixels, so a fractional scale factor
        // rarely round-trips to the exact logical size that was asked for.
        if (now.width - width).abs() <= 1.0 && (now.height - height).abs() <= 1.0 {
            return;
        }
        sleep_ms(FIT_POLL_MS).await;
    }
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
// Start with the OS
// ---------------------------------------------------------------------------------------------

/// Whether this build can put the app in the reader's sign-in list at all.
///
/// False leaves the switch out of the settings sheet rather than showing one that does nothing.
///
/// What the implementations below register is the running binary's own path, never the install
/// directory the installer would have used: this app also ships as a portable archive and as an
/// `AppImage`, and both run from wherever the reader put them.
pub(crate) fn autostart_supported() -> bool {
    autostart::SUPPORTED
}

/// Whether the app is currently registered to start at sign-in.
///
/// **Read from the OS, not from the settings file.** The Windows installer offers the same choice
/// as a checkbox and writes the same registry value, so a mirror in `settings.json` would be a
/// second opinion that disagrees the moment the reader uses the other one — and the OS is the
/// copy that decides what actually happens.
pub(crate) fn autostart_enabled() -> bool {
    autostart::enabled()
}

/// Register or deregister the app for sign-in. Returns whether the OS accepted it.
///
/// Unlike the rest of this module a refusal is *not* silent: this one is a switch the reader
/// flipped and watched, so the caller puts the toggle back where it was and says so.
pub(crate) fn set_autostart(enabled: bool) -> bool {
    autostart::set(enabled)
}

/// `HKCU\…\Run`, the one supported way to start an app at sign-in without a shell shortcut — and
/// a shortcut is a COM object, which a crate that forbids `unsafe` cannot build.
#[cfg(windows)]
mod autostart {
    pub(super) const SUPPORTED: bool = true;

    /// The value name, and what Task Manager's Startup tab lists the entry as.
    ///
    /// **`bundle/windows/installer.nsi.hbs` writes this exact name at this exact key**, so the
    /// installer checkbox and this switch are two views of one value rather than two settings
    /// that could disagree. Renaming either side alone orphans whatever the other one wrote,
    /// leaving the app starting at sign-in with nothing in the UI admitting it.
    const VALUE: &str = "TankoVault";
    const KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

    pub(super) fn enabled() -> bool {
        windows_registry::CURRENT_USER
            .open(KEY)
            .and_then(|key| key.get_string(VALUE))
            .is_ok_and(|command| !command.trim().is_empty())
    }

    pub(super) fn set(enabled: bool) -> bool {
        // `create` rather than `open`: the Run key exists on every Windows install, but opening
        // for write is the same call either way and this one cannot fail for a missing key.
        let Ok(key) = windows_registry::CURRENT_USER.create(KEY) else {
            return false;
        };
        if enabled {
            let Ok(command) = std::env::current_exe() else {
                return false;
            };
            // Quoted, because a path with a space in it — `C:\Program Files\…` under a
            // per-machine install — is otherwise read as a program name plus arguments.
            key.set_string(VALUE, format!("\"{}\"", command.display()))
                .is_ok()
        } else {
            match key.remove_value(VALUE) {
                Ok(()) => true,
                // `remove_value` reports "no such value" as an error, and a value that was never
                // there is the state being asked for. Re-read rather than match on the code.
                Err(_) => key.get_string(VALUE).is_err(),
            }
        }
    }
}

/// The freedesktop autostart directory: a `.desktop` entry in `$XDG_CONFIG_HOME/autostart` is
/// what every conforming session reads at login.
#[cfg(all(unix, not(target_vendor = "apple")))]
mod autostart {
    use std::path::PathBuf;

    pub(super) const SUPPORTED: bool = true;

    /// Matches the bundle identifier, which is how a desktop environment keys the entry back to
    /// the installed application.
    const ENTRY_FILE_NAME: &str = "dev.tankovault.frontend.desktop";

    fn entry_path() -> Option<PathBuf> {
        directories::BaseDirs::new()
            .map(|dirs| dirs.config_dir().join("autostart").join(ENTRY_FILE_NAME))
    }

    pub(super) fn enabled() -> bool {
        entry_path().is_some_and(|path| path.is_file())
    }

    pub(super) fn set(enabled: bool) -> bool {
        let Some(path) = entry_path() else {
            return false;
        };
        if !enabled {
            // Same reading as the Windows side: an entry that is not there is the state asked
            // for, not a failure.
            return match std::fs::remove_file(&path) {
                Ok(()) => true,
                Err(error) => error.kind() == std::io::ErrorKind::NotFound,
            };
        }
        // `APPIMAGE` before `current_exe`, and it is not a nicety: inside an `AppImage` the
        // running binary is a file on a temporary mount that is gone by the next login, so a
        // session started from one would autostart a path that no longer exists. The runtime sets
        // this to the image itself, which is what the reader would double-click.
        let Some(command) = std::env::var_os("APPIMAGE")
            .map(PathBuf::from)
            .or_else(|| std::env::current_exe().ok())
        else {
            return false;
        };
        let Some(dir) = path.parent() else {
            return false;
        };
        if std::fs::create_dir_all(dir).is_err() {
            return false;
        }
        // `Exec` is a shell-like word list, so a path containing a space has to be quoted for the
        // same reason the registry command does.
        let entry = format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=TankoVault\n\
             Exec=\"{}\"\n\
             Terminal=false\n\
             X-GNOME-Autostart-enabled=true\n",
            command.display()
        );
        std::fs::write(&path, entry).is_ok()
    }
}

/// No third desktop platform is shipped — the release workflow builds Windows and Linux — so this
/// exists to keep the module compiling on a developer's macOS rather than to serve anyone. The
/// settings sheet reads [`SUPPORTED`] and leaves the switch out entirely.
#[cfg(not(any(windows, all(unix, not(target_vendor = "apple")))))]
mod autostart {
    pub(super) const SUPPORTED: bool = false;

    pub(super) fn enabled() -> bool {
        false
    }

    pub(super) fn set(_enabled: bool) -> bool {
        false
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Work areas the sweeps below stand in for: a laptop, two desktop sizes and an ultrawide.
    ///
    /// Whole-monitor figures, which is the *worst* case for these bounds — a real work area is
    /// smaller, so anything that fits inside these fits inside that.
    const DISPLAYS: [(f64, f64); 5] = [
        (1366.0, 768.0),
        (1920.0, 1080.0),
        (2560.0, 1440.0),
        (3440.0, 1440.0),
        (5120.0, 2160.0),
    ];

    /// The bug the fit gate exists for. The window opens at [`STARTUP_INNER_SIZE`] and is resized
    /// to the display a moment later, so anything that measured itself in between read a geometry
    /// the reader never sees. Discover derives its page size from exactly such a measurement
    /// (`crate::components::use_grid_fit`): it fetched a 1280px window's worth of covers and then
    /// laid them into the fitted window's wider grid, leaving a short row and a band of dead space
    /// at every page boundary.
    ///
    /// That the two sizes differ on an ordinary display is what makes the window real. If they
    /// ever agreed, `fit_window_to_display`'s await would be silently untested rather than
    /// unnecessary — so this pins the difference, not the numbers.
    #[test]
    fn the_fitted_size_is_never_the_size_the_window_opens_at() {
        for display in DISPLAYS {
            let (width, _) = fitted_inner_size(display).expect("a real monitor is fitted to");
            assert!(
                (width - STARTUP_INNER_SIZE.0).abs() > 1.0,
                "a {display:?} display fits to the placeholder width"
            );
        }
    }

    /// The window always fits the area it is given, so a laptop is never handed a window taller
    /// than the space left for it, and the width ceiling is never exceeded on an ultrawide.
    #[test]
    fn the_window_fits_the_work_area_and_stays_under_the_ceiling() {
        for display in DISPLAYS {
            let (width, height) = fitted_inner_size(display).expect("a real monitor is fitted to");
            assert!(
                width <= display.0 && height <= display.1,
                "{display:?} is handed a window bigger than itself"
            );
            assert!(
                width <= widest_useful_window(),
                "{display:?} is handed a window past the ceiling"
            );
        }
    }

    /// The bug the ceiling is a sum for. It used to be a flat `1760` — the cap on the *content
    /// column* — applied to the *window*, which also carries the rail, both gutters and the body's
    /// scrollbar. That left the column 335px short of its own cap: five covers across Discover
    /// where the layout fits seven. A ceiling has to be stated in the units of the thing it bounds,
    /// so this pins that the window it allows is one the content column can actually fill.
    #[test]
    fn the_ceiling_leaves_room_for_the_chrome_around_the_content_column() {
        let column = widest_useful_window() - RAIL_WIDTH - 2.0 * CONTENT_GUTTER - SCROLLBAR_WIDTH;
        assert!(
            (column - WIDEST_MEASURE).abs() <= 1.0,
            "the ceiling gives the content column {column}px, not the {WIDEST_MEASURE}px it caps at"
        );
    }

    /// Height is bounded by the work area and nothing else. A flat 1120px cap cost a 1440p
    /// display 60px of covers and a 4K one 1000px, for no layout reason: no band inside
    /// `.ik-main` caps its own height, so a taller window is simply more rows.
    #[test]
    fn a_tall_display_is_not_capped_to_a_fixed_height() {
        let (_, tall) = fitted_inner_size((3840.0, 2160.0)).expect("a real monitor is fitted to");
        let (_, short) = fitted_inner_size((1920.0, 1080.0)).expect("a real monitor is fitted to");
        assert!(
            tall > short * 1.9,
            "a display twice as tall yields {tall}px against {short}px"
        );
    }

    /// A monitor that reports no usable size leaves the window alone rather than collapsing it —
    /// the placeholder is a worse answer than nothing, but a zero-sized window is worse than both.
    #[test]
    fn a_monitor_with_no_size_is_not_fitted_to() {
        assert!(fitted_inner_size((0.0, 1080.0)).is_none());
        assert!(fitted_inner_size((1920.0, 0.0)).is_none());
    }
}
