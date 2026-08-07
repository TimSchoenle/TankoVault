//! The desktop client's own settings, reached from the window header.
//!
//! Everything here is a property of *this installation*, not of the account: which server it
//! talks to, and whether pushes raise an OS notification. Account settings stay on the account
//! screen, where they belong and where they can be synced.
//!
//! **It is deliberately outside the router, and outside the sign-in gate.** The server address
//! is the one setting a reader needs precisely when nothing else works — a typo, a moved host, a
//! server that is down — and every routed screen needs a working server to render. Putting it
//! behind `AuthRequired`, as an earlier revision did, meant a wrong address could only be
//! corrected by deleting the settings file by hand.

use crate::components::Field;
use crate::i18n::use_i18n;
use crate::icons::{Ic, Icon};
use dioxus::prelude::*;

/// The settings sheet. `on_close` dismisses it.
#[component]
pub(crate) fn SettingsSheet(on_close: EventHandler<()>) -> Element {
    let i18n = use_i18n();

    rsx! {
        div {
            class: "ik-prefs-scrim",
            // Click-outside to dismiss, and `Escape` on the sheet itself below. The scrim is a
            // presentational element, so it carries no role — the dismiss it offers is a
            // convenience duplicated by a real button.
            onclick: move |_| on_close.call(()),
            div {
                class: "ik-prefs",
                role: "dialog",
                "aria-modal": "true",
                "aria-label": i18n.t("settings.title"),
                // Or a click on the sheet would bubble to the scrim and close it.
                onclick: move |event| event.stop_propagation(),
                onkeydown: move |event| {
                    if event.key() == Key::Escape {
                        on_close.call(());
                    }
                },
                div { class: "ik-prefs-head",
                    Ic { icon: Icon::Settings, size: 17 }
                    strong { {i18n.t("settings.title")} }
                    button {
                        class: "ik-prefs-close",
                        r#type: "button",
                        "aria-label": i18n.t("common.close"),
                        onclick: move |_| on_close.call(()),
                        Ic { icon: Icon::Close, size: 15 }
                    }
                }

                ServerSection {}
                NotificationSection {}

                if let Some(path) = crate::platform::settings_path() {
                    p { class: "ik-muted", style: "font-size:11.5px;word-break:break-all;margin:4px 0 0;",
                        {i18n.args("connect.storedAt", &[("path", &path.display().to_string())])}
                    }
                }
            }
        }
    }
}

/// Which server this installation talks to.
///
/// Changing it signs the reader out, and that is not a courtesy: the access token in memory was
/// minted by the *old* server and means nothing to the new one, so keeping it would send a
/// stranger's deployment a credential and then show a wall of 401s it could not explain.
#[component]
fn ServerSection() -> Element {
    let i18n = use_i18n();
    let api = crate::api::use_api();
    let session = crate::state::use_session();
    let current = crate::platform::server_origin().unwrap_or_default();
    let mut entered = use_signal(|| current.clone());
    let mut error = use_signal(|| Option::<String>::None);
    let mut probing = use_signal(|| false);

    let mut change = move |()| {
        if *probing.peek() {
            return;
        }
        let candidate = match crate::views::connect::normalise(&entered.peek().clone()) {
            Ok(origin) => origin,
            Err(key) => {
                error.set(Some(i18n.t(key)));
                return;
            }
        };
        error.set(None);
        probing.set(true);
        spawn(async move {
            match crate::views::connect::probe(&candidate).await {
                Ok(()) => {
                    crate::platform::set_server_origin(Some(&candidate));
                    api.set_base(&candidate);
                    session.clear();
                    probing.set(false);
                }
                Err(key) => {
                    error.set(Some(i18n.t(key)));
                    probing.set(false);
                }
            }
        });
    };

    rsx! {
        section { class: "ik-prefs-section",
            h3 { {i18n.t("connect.card.title")} }
            p { class: "ik-muted", style: "font-size:12.5px;margin-top:0;",
                {i18n.t("connect.card.intro")}
            }
            if let Some(message) = error.read().clone() {
                div { class: "ik-error", style: "padding:10px;margin-bottom:10px;", "{message}" }
            }
            Field {
                id: "tv-settings-origin",
                label: i18n.t("connect.field.server"),
                kind: "url",
                value: entered(),
                on_input: move |value| entered.set(value),
                on_enter: change,
            }
            div { class: "ik-prefs-actions",
                button {
                    class: "ik-btn primary",
                    r#type: "button",
                    disabled: probing() || *entered.read() == current,
                    onclick: move |_| change(()),
                    if probing() {
                        {i18n.t("connect.connecting")}
                    } else {
                        {i18n.t("connect.card.action")}
                    }
                }
                // The way out when the stored address answers nothing at all, so no probe can
                // ever succeed and the button above can never be pressed.
                button {
                    class: "ik-btn",
                    r#type: "button",
                    disabled: probing(),
                    onclick: move |_| {
                        crate::platform::set_server_origin(None);
                        session.clear();
                    },
                    {i18n.t("settings.forgetServer")}
                }
            }
        }
    }
}

/// Whether a push raises an OS notification.
#[component]
fn NotificationSection() -> Element {
    let i18n = use_i18n();
    let mut enabled = use_signal(crate::platform::notifications_enabled);

    rsx! {
        section { class: "ik-prefs-section",
            h3 { {i18n.t("settings.notifications.title")} }
            label { class: "ik-prefs-toggle",
                input {
                    r#type: "checkbox",
                    checked: enabled(),
                    onchange: move |event| {
                        let on = event.checked();
                        crate::platform::set_notifications_enabled(on);
                        enabled.set(on);
                    },
                }
                span { {i18n.t("settings.notifications.label")} }
            }
            p { class: "ik-muted", style: "font-size:12.5px;margin:6px 0 0;",
                {i18n.t("settings.notifications.hint")}
            }
        }
    }
}
