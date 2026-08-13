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

use crate::i18n::use_i18n;
use crate::platform::{self, Tray, TrayCommand};
use dioxus::prelude::*;

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
    // Captured once, at mount: `platform::window` reads a context, and the subscription below
    // fires from the event loop, where that lookup would answer `None`.
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

    platform::use_tray_commands(tray, move |command| {
        let Some(window) = window.as_ref() else {
            return;
        };
        match command {
            TrayCommand::Open => platform::show_window(window),
            TrayCommand::Quit => platform::quit_app(window),
        }
    });

    rsx! {}
}
