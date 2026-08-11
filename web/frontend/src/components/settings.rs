//! The desktop client's own settings, reached from the window header.
//!
//! Everything here is a property of *this installation*, not of the account: which server it
//! talks to, how its window behaves, whether pushes raise an OS notification, and how it keeps
//! itself up to date. Account settings stay on the account screen, where they belong and where
//! they can be synced. A reader with two machines may reasonably want one of them current and
//! the other pinned.
//!
//! **It is deliberately outside the router, and outside the sign-in gate.** The server address
//! is the one setting a reader needs precisely when nothing else works — a typo, a moved host, a
//! server that is down — and every routed screen needs a working server to render. Putting it
//! behind `AuthRequired`, as an earlier revision did, meant a wrong address could only be
//! corrected by deleting the settings file by hand. It is also why [`Category::Server`] is the
//! one the sheet opens on.
//!
//! The categories are a real division and not a longer sheet folded up: each one answers a
//! different question ("what does it talk to", "what does the window do", "what may interrupt
//! me", "what version am I on"), and the sheet used to answer all four in one scrolling column
//! where the last of them was below the fold.

use crate::components::{CloseToTray, Field, SegControl, SliderRow, TabBar, TabKind};
use crate::i18n::use_i18n;
use crate::icons::{Ic, Icon};
use crate::update::{self, Policy, Status, UpdateState};
use dioxus::prelude::*;
use inkstone_ui::{Button, Tone};
/// One group of settings. See the module note on why this is a division rather than a fold.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Category {
    Server,
    Window,
    Notifications,
    Updates,
    About,
}

impl TabKind for Category {
    fn all() -> &'static [Self] {
        &[
            Self::Server,
            Self::Window,
            Self::Notifications,
            Self::Updates,
            Self::About,
        ]
    }

    fn label_key(self) -> &'static str {
        match self {
            Self::Server => "settings.tab.server",
            Self::Window => "settings.tab.window",
            Self::Notifications => "settings.tab.notifications",
            Self::Updates => "settings.tab.updates",
            Self::About => "settings.tab.about",
        }
    }
}

/// The settings sheet. `on_close` dismisses it.
#[component]
pub(crate) fn SettingsSheet(on_close: EventHandler<()>) -> Element {
    let i18n = use_i18n();
    let mut category = use_signal(|| Category::Server);
    let update = use_context::<UpdateState>();

    // Every way out of the sheet goes through here, so the "updated to …" receipt is retired
    // exactly once and by the reader having seen it — see `update::acknowledge_applied`.
    let close = move |()| {
        update::acknowledge_applied(update);
        on_close.call(());
    };

    rsx! {
        div {
            class: "ik-prefs-scrim",
            // Click-outside to dismiss, and `Escape` on the sheet itself below. The scrim is a
            // presentational element, so it carries no role — the dismiss it offers is a
            // convenience duplicated by a real button.
            onclick: move |_| close(()),
            div {
                class: "ik-prefs",
                role: "dialog",
                "aria-modal": "true",
                "aria-label": i18n.t("settings.title"),
                // Or a click on the sheet would bubble to the scrim and close it.
                onclick: move |event| event.stop_propagation(),
                onkeydown: move |event| {
                    if event.key() == Key::Escape {
                        close(());
                    }
                },
                div { class: "ik-prefs-head",
                    Ic { icon: Icon::Settings, size: 17 }
                    strong { {i18n.t("settings.title")} }
                    button {
                        class: "ik-prefs-close",
                        r#type: "button",
                        "aria-label": i18n.t("common.close"),
                        onclick: move |_| close(()),
                        Ic { icon: Icon::Close, size: 15 }
                    }
                }

                TabBar {
                    selected: category(),
                    on_select: move |next| category.set(next),
                }

                match category() {
                    Category::Server => rsx! { ServerSection {} },
                    Category::Window => rsx! { WindowSection {} },
                    Category::Notifications => rsx! { NotificationSection {} },
                    Category::Updates => rsx! { UpdateSection {} },
                    Category::About => rsx! { AboutSection {} },
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
                Button {
                    tone: Tone::Primary,
                    disabled: probing() || *entered.read() == current,
                    on_click: move |_| change(()),
                    if probing() {
                    {i18n.t("connect.connecting")}
                    } else {
                    {i18n.t("connect.card.action")}
                    }
                }
                // The way out when the stored address answers nothing at all, so no probe can
                // ever succeed and the button above can never be pressed.
                Button {
                    disabled: probing(),
                    on_click: move |_| {
                        crate::platform::set_server_origin(None);
                        session.clear();
                    },
                    {i18n.t("settings.forgetServer")}
                }
            }
        }
    }
}

/// What the window does at either end of a session: whether the app is started with the reader's,
/// and whether closing it ends the app.
///
/// Both switches are absent where the platform has nothing behind them rather than shown and
/// inert, so what is on this tab is what this machine can actually do.
#[component]
fn WindowSection() -> Element {
    let i18n = use_i18n();
    let startup = crate::platform::autostart_supported();
    let tray = crate::platform::tray_supported();

    rsx! {
        if startup {
            StartupSection {}
        }
        if tray {
            CloseToTraySection {}
        }
        if !startup && !tray {
            p { class: "ik-muted", style: "font-size:12.5px;margin:0;",
                {i18n.t("settings.window.unsupported")}
            }
        }
    }
}

/// Whether the app is in the reader's sign-in list.
///
/// The one switch on this sheet whose state is *not* stored by the app: it reads and writes the
/// OS list directly (`HKCU\…\Run`, or a freedesktop `autostart` entry), because the Windows
/// installer offers the same choice as a checkbox and both have to mean the same thing. That is
/// also why a refusal is shown rather than swallowed — the reader can see the box move, so a
/// change that did not take has to say so.
#[component]
fn StartupSection() -> Element {
    let i18n = use_i18n();
    let mut enabled = use_signal(crate::platform::autostart_enabled);
    let mut refused = use_signal(|| false);

    rsx! {
        section { class: "ik-prefs-section",
            h3 { {i18n.t("settings.startup.title")} }
            label { class: "ik-prefs-toggle",
                input {
                    r#type: "checkbox",
                    checked: enabled(),
                    onchange: move |event| {
                        let on = event.checked();
                        let applied = crate::platform::set_autostart(on);
                        // On refusal the OS list is unchanged, so the switch goes back to what it
                        // actually reflects rather than to what was asked for.
                        enabled.set(if applied { on } else { !on });
                        refused.set(!applied);
                    },
                }
                span { {i18n.t("settings.startup.label")} }
            }
            p { class: "ik-muted", style: "font-size:12.5px;margin:6px 0 0;",
                {i18n.t("settings.startup.hint")}
            }
            if refused() {
                div {
                    class: "ik-error",
                    role: "alert",
                    style: "padding:10px;margin-top:8px;font-size:12.5px;",
                    {i18n.t("settings.startup.failed")}
                }
            }
        }
    }
}

/// Whether the close button ends the app or leaves it in the tray.
///
/// The switch writes the setting *and* the shared signal, and the second one is what actually
/// changes anything: `components::tray::TrayHost` watches it, puts the icon in the tray and
/// tells the window to hide rather than close. Writing only the setting would take effect at the
/// next start — which is exactly the shape of bug that gets reported as "the option does
/// nothing".
///
/// Only rendered where [`crate::platform::tray_supported`] is true, so there is never a window
/// that hides with nothing left to bring it back.
#[component]
fn CloseToTraySection() -> Element {
    let i18n = use_i18n();
    let mut enabled = use_context::<CloseToTray>().0;

    rsx! {
        section { class: "ik-prefs-section",
            h3 { {i18n.t("settings.window.closing.title")} }
            label { class: "ik-prefs-toggle",
                input {
                    r#type: "checkbox",
                    checked: enabled(),
                    onchange: move |event| {
                        let on = event.checked();
                        crate::platform::set_close_to_tray(on);
                        enabled.set(on);
                    },
                }
                span { {i18n.t("settings.window.closing.label")} }
            }
            p { class: "ik-muted", style: "font-size:12.5px;margin:6px 0 0;",
                {i18n.t("settings.window.closing.hint")}
            }
        }
    }
}

/// Whether a push raises an OS notification, and a way to find out whether one arrives.
///
/// The test button is not padding. A chapter landing is the *only* thing that raises a
/// notification here, which can be days apart, so without it "did I set this up correctly" is a
/// question the reader cannot answer — and there are now two switches that have to agree: this
/// one and the operating system's own per-app control (see `platform::desktop`'s `identify`).
/// One press exercises the whole path either would break.
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
            p { class: "ik-muted", style: "font-size:12.5px;margin:6px 0 0;",
                {i18n.t("settings.notifications.systemHint")}
            }
            div { class: "ik-prefs-actions", style: "margin-top:10px;",
                Button {
                    disabled: !enabled(),
                    on_click: move |_| {
                        crate::platform::notify(
                            &i18n.t("settings.notifications.test.title"),
                            &i18n.t("settings.notifications.test.body"),
                        );
                    },
                    {i18n.t("settings.notifications.test.action")}
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

    let api = crate::api::use_api();
    let check = move |()| {
        spawn(async move { update::check(state, api, i18n).await });
    };

    rsx! {
        section { class: "ik-prefs-section",
            h3 { {i18n.t("settings.update.title")} }

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
                // The one state with a number behind it. A percentage in a sentence is easy to
                // miss on a long download, and this is the whole of the feedback an automatic
                // update gives before it goes quiet again.
                if let Status::Downloading { percent } = status {
                    div {
                        class: "ik-progress",
                        style: "margin-top:8px;",
                        role: "progressbar",
                        "aria-valuenow": "{percent}",
                        "aria-valuemin": "0",
                        "aria-valuemax": "100",
                        "aria-label": i18n.t("settings.update.title"),
                        span { style: "width:{percent}%;" }
                    }
                }
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
    let api = crate::api::use_api();

    rsx! {
        div { class: "ik-prefs-actions",
            Button {
                disabled: busy,
                on_click: move |_| on_check.call(()),
                {i18n.t("settings.update.check")}
            }
            match status {
                Status::Available { installable: true, .. } => rsx! {
                    Button {
                        tone: Tone::Primary,
                        disabled: busy,
                        on_click: move |_| {
                            spawn(async move { update::install_now(state, api, i18n).await });
                        },
                        {i18n.t("settings.update.install")}
                    }
                    Button {
                        on_click: move |_| update::dismiss(state),
                        {i18n.t("settings.update.dismiss")}
                    }
                },
                Status::Available { page, installable: false, .. } => rsx! {
                    Button {
                        on_click: move |_| crate::platform::navigate_to(&page),
                        {i18n.t("settings.update.openPage")}
                    }
                },
                // Applying happens at the next start, so the only thing left to offer is the
                // restart itself.
                //
                // `quit_app`, never `window.close()`: with close-to-tray on, closing hides the
                // window, the process lives on and the update the reader just asked for is
                // applied at some unrelated start days later.
                Status::Staged { .. } => rsx! {
                    Button {
                        tone: Tone::Primary,
                        on_click: move |_| {
                            if let Some(window) = crate::platform::window() {
                                crate::platform::quit_app(&window);
                            }
                        },
                        {i18n.t("settings.update.quit")}
                    }
                },
                Status::Idle
                | Status::Checking
                | Status::UpToDate
                | Status::Downloading { .. }
                | Status::Applied { .. }
                | Status::Unsupported { .. }
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
        Status::Applied { version } => {
            i18n.args("settings.update.state.applied", &[("version", version)])
        }
        Status::Unsupported { version, supported } => i18n.args(
            "settings.update.state.unsupported",
            &[("version", version), ("supported", supported)],
        ),
        Status::Failed(key) => i18n.t(key),
    }
}

/// What this copy is: its version, and where it keeps what the reader has told it.
///
/// The settings path is here rather than under [`Category::Server`] where it used to sit,
/// because it is the answer to "what would I delete to start over" — a question about the
/// installation, not about the server it happens to point at.
#[component]
fn AboutSection() -> Element {
    let i18n = use_i18n();
    let branding = crate::state::branding::use_branding();

    rsx! {
        section { class: "ik-prefs-section",
            h3 { {i18n.t("settings.about.title")} }
            p { style: "font-size:12.5px;margin:0;",
                span { "v{crate::build_info::VERSION}" }
                if let Some(commit) = crate::build_info::commit() {
                    span { class: "ik-muted", " · {commit}" }
                }
            }
            if let Some(path) = crate::platform::settings_path() {
                p { class: "ik-muted", style: "font-size:11.5px;word-break:break-all;margin:10px 0 0;",
                    {i18n.args("connect.storedAt", &[("path", &path.display().to_string())])}
                }
            }
            div { class: "ik-prefs-actions", style: "margin-top:12px;",
                Button {
                    // The deployment's own project, not this one's: a fork's About tab must
                    // not send its readers here.
                    on_click: move |_| crate::platform::navigate_to(&branding.read().project_url),
                    {i18n.t("settings.about.project")}
                }
            }
        }
    }
}
