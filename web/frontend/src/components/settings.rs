//! The desktop client's own settings, reached from the window header.
//!
//! Everything here is a property of *this installation*, not of the account: which server it
//! talks to, whether pushes raise an OS notification, and how it keeps itself up to date. Account
//! settings stay on the account screen, where they belong and where they can be synced. A reader
//! with two machines may reasonably want one of them current and the other pinned.
//!
//! **It is deliberately outside the router, and outside the sign-in gate.** The server address
//! is the one setting a reader needs precisely when nothing else works — a typo, a moved host, a
//! server that is down — and every routed screen needs a working server to render. Putting it
//! behind `AuthRequired`, as an earlier revision did, meant a wrong address could only be
//! corrected by deleting the settings file by hand.

use crate::components::{Field, SegControl, SliderRow};
use crate::i18n::use_i18n;
use crate::icons::{Ic, Icon};
use crate::update::{self, Policy, Status, UpdateState};
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
                UpdateSection {}
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

/// How this installation keeps itself current (`crate::update`).
///
/// Three states this section has to render honestly rather than hide, because each one means the
/// reader will not be updated and would otherwise be left guessing:
///
/// * **this build carries no signing key**, so nothing could be verified and no check is made;
/// * **this copy is not ours to replace** — installed from the `.deb`, or run from the portable
///   archive — so a release is announced and never applied;
/// * **the release on offer has no signed manifest**, which is every release cut before this
///   feature existed.
#[component]
fn UpdateSection() -> Element {
    let i18n = use_i18n();
    let state = use_context::<UpdateState>();
    let mut policy = use_signal(update::policy);
    let mut hold_back = use_signal(update::min_age_days);
    // Fixed for the life of the process: it is decided by where this executable sits.
    let flavour = use_hook(update::flavour);
    let status = state.status();
    let busy = matches!(status, Status::Checking | Status::Downloading { .. });

    let check = move |()| {
        spawn(async move { update::check(state, i18n).await });
    };

    rsx! {
        section { class: "ik-prefs-section",
            h3 { {i18n.t("settings.update.title")} }

            p { class: "ik-muted", style: "font-size:12.5px;margin:0 0 12px;",
                span { "v{crate::build_info::VERSION}" }
                if let Some(commit) = crate::build_info::commit() {
                    span { " · {commit}" }
                }
            }

            if update::is_configured() {
                div { class: "ik-subhead", style: "margin-bottom:8px;", {i18n.t("settings.update.policy")} }
                SegControl {
                    options: update::Policy::all()
                        .iter()
                        .map(|option| (option.token().to_owned(), i18n.t(option.label_key())))
                        .collect::<Vec<_>>(),
                    selected: policy().token().to_owned(),
                    on_select: move |token: String| {
                        if let Some(chosen) = Policy::from_token(&token) {
                            update::set_policy(chosen);
                            policy.set(chosen);
                        }
                    },
                }
                if policy() != Policy::Off {
                    div { style: "margin-top:12px;",
                        SliderRow {
                            label: i18n.t("settings.update.holdBack"),
                            value: f64::from(hold_back()),
                            min: 0.0,
                            max: f64::from(update::MAX_MIN_AGE_DAYS),
                            step: 1.0,
                            display: i18n.plural("settings.update.days", i64::from(hold_back()), &[]),
                            on_input: move |position: f64| {
                                let days = update::days_from_slider(position);
                                update::set_min_age_days(days);
                                hold_back.set(days);
                            },
                        }
                    }
                    p { class: "ik-muted", style: "font-size:12.5px;margin:6px 0 0;",
                        {i18n.t("settings.update.holdBackHint")}
                    }
                }
                if let Some(reason) = flavour.unmanaged_reason() {
                    p { class: "ik-muted", style: "font-size:12.5px;margin:10px 0 0;", {i18n.t(reason)} }
                }
                p { class: "ik-muted", style: "font-size:12.5px;margin:10px 0 0;",
                    {i18n.t("settings.update.source")}
                }
                p { style: "font-size:12.5px;margin:10px 0 0;", {state_text(&status, i18n)} }
                UpdateActions { state, status: status.clone(), busy, on_check: check }
            } else {
                p { class: "ik-muted", style: "font-size:12.5px;margin:10px 0 0;",
                    {i18n.t("settings.update.error.unconfigured")}
                }
            }
        }
    }
}

/// What the reader can do about the state the updater is in.
///
/// Split out so the branching lives in one place: an `Available` release is offered a download
/// only when this app could actually apply it, and the release page otherwise.
#[component]
fn UpdateActions(
    state: UpdateState,
    status: Status,
    busy: bool,
    on_check: EventHandler<()>,
) -> Element {
    let i18n = use_i18n();

    rsx! {
        div { class: "ik-prefs-actions",
            button {
                class: "ik-btn",
                r#type: "button",
                disabled: busy,
                onclick: move |_| on_check.call(()),
                {i18n.t("settings.update.check")}
            }
            match status {
                Status::Available { installable: true, .. } => rsx! {
                    button {
                        class: "ik-btn primary",
                        r#type: "button",
                        disabled: busy,
                        onclick: move |_| {
                            spawn(async move { update::install_now(state, i18n).await });
                        },
                        {i18n.t("settings.update.install")}
                    }
                    button {
                        class: "ik-btn",
                        r#type: "button",
                        onclick: move |_| update::dismiss(state),
                        {i18n.t("settings.update.dismiss")}
                    }
                },
                Status::Available { page, installable: false, .. } => rsx! {
                    button {
                        class: "ik-btn",
                        r#type: "button",
                        onclick: move |_| crate::platform::navigate_to(&page),
                        {i18n.t("settings.update.openPage")}
                    }
                },
                // Applying happens at the next start, so the only thing left to offer is the
                // restart itself — closing the window is what gets there.
                Status::Staged { .. } => rsx! {
                    button {
                        class: "ik-btn primary",
                        r#type: "button",
                        onclick: move |_| {
                            if let Some(window) = crate::platform::window() {
                                window.close();
                            }
                        },
                        {i18n.t("settings.update.quit")}
                    }
                },
                Status::Idle
                | Status::Checking
                | Status::UpToDate
                | Status::Downloading { .. }
                | Status::Failed(_) => rsx! {},
            }
        }
    }
}

/// The one-line description of what the updater is doing. `Failed` carries a catalogue key, so it
/// resolves the same way as every other line here.
fn state_text(status: &Status, i18n: crate::i18n::Translator) -> String {
    match status {
        Status::Idle => i18n.t("settings.update.state.idle"),
        Status::Checking => i18n.t("settings.update.state.checking"),
        Status::UpToDate => i18n.t("settings.update.state.upToDate"),
        Status::Available {
            version,
            installable,
            ..
        } => {
            let key = if *installable {
                "settings.update.state.available"
            } else {
                "settings.update.state.availableOnly"
            };
            i18n.args(key, &[("version", version)])
        }
        Status::Downloading { percent } => i18n.args(
            "settings.update.state.downloading",
            &[("percent", &percent.to_string())],
        ),
        Status::Staged { version } => {
            i18n.args("settings.update.state.staged", &[("version", version)])
        }
        Status::Failed(key) => i18n.t(key),
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
