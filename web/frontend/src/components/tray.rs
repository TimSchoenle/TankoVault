//! The tray icon's lifecycle, for the desktop build only.
//!
//! Renders nothing. It exists because a tray icon is a *resource* with the lifetime of a
//! component: [`crate::platform::Tray`] removes the icon when it is dropped, so holding it in a
//! signal is what lets the settings switch take effect without a restart.
//!
//! It also owns the two consequences of that switch, which are easy to separate and must not be:
//! the window hides instead of closing **and** there is something in the tray to bring it back.
//! Either one alone is a defect — a hidden window with no tray entry is an app that has to be
//! killed from the task manager — so both are decided here, from one signal.
//!
//! The loop below is not the tray's at all: a launch refused by the single-instance lock leaves
//! an activation request behind, and this is where it is answered. It lives here because raising
//! the window needs the `DesktopContext` captured at mount, which this component already holds
//! for the tray's own Open entry — a second component would exist only to capture the same
//! handle again.

use crate::i18n::use_i18n;
use crate::platform::{self, Tray, TrayCommand};
use dioxus::prelude::*;

/// How long the activation loop waits between checks. This is the delay a reader waits between
/// launching the app a second time and their existing window coming forward. Half a second reads
/// as "it opened", and the poll it paces is one `remove_file` on a path that is almost always
/// absent.
const ACTIVATION_POLL_MS: u32 = 500;

/// The reader's close-to-tray choice: written by the settings sheet, acted on by [`TrayHost`].
///
/// A context rather than a read of the settings file on both sides, because the two have to move
/// together within one session — the sheet is what the reader watches, and the tray is what makes
/// the answer true.
#[derive(Clone, Copy)]
pub(crate) struct CloseToTray(pub(crate) Signal<bool>);

impl CloseToTray {
    pub(crate) fn new() -> Self {
        Self(Signal::new(platform::close_to_tray_enabled()))
    }
}

/// Keeps the tray icon and the window's close behaviour in step with [`CloseToTray`].
#[component]
pub(crate) fn TrayHost() -> Element {
    let i18n = use_i18n();
    let enabled = use_context::<CloseToTray>().0;
    // Captured once, at mount: `platform::window` reads a context, and the tray subscription and
    // the task below both run where that lookup would answer `None`.
    let window = crate::platform::window();
    let mut tray = use_signal(|| Option::<Tray>::None);

    let held = window.clone();
    use_effect(move || {
        let Some(window) = held.as_ref() else {
            return;
        };
        if enabled() {
            // Rebuilt rather than reused when the labels change with the reader's language: a
            // tray menu's text is set at build time by both backends.
            tray.set(Tray::install(
                &i18n.t("settings.window.tray.open"),
                &i18n.t("settings.window.tray.quit"),
            ));
        } else {
            tray.set(None);
        }
        // Read from the signal, not from the request: an install the platform refused leaves the
        // close button meaning *close*, which is the only honest answer when nothing would be
        // left to restore the window from.
        platform::set_window_hides_on_close(window, tray.peek().is_some());
    });

    let held = window.clone();
    platform::use_tray_commands(tray, move |command| {
        let Some(window) = held.as_ref() else {
            return;
        };
        match command {
            TrayCommand::Open => platform::show_window(window),
            TrayCommand::Quit => platform::quit_app(window),
        }
    });

    use_future(move || {
        let window = window.clone();
        async move {
            loop {
                // Answered whether or not the reader turned the icon on: the lock refuses a
                // duplicate launch either way.
                if platform::take_activation_request() {
                    if let Some(window) = window.as_ref() {
                        platform::show_window(window);
                    }
                }
                platform::sleep_ms(ACTIVATION_POLL_MS).await;
            }
        }
    });

    rsx! {}
}
