//! Account & settings (DESIGN_SPEC §7.7) — a settings shell with a sub-nav and panels.
//! **Profile** (`PATCH /v1/me/profile`), **Appearance** (`localStorage` + `data-*`),
//! **Security & sessions** (`GET`/`DELETE /v1/me/sessions`) and **Notification prefs**
//! (`GET`/`PUT /v1/me/notification-prefs`) are wired for real (§9.4); **Sync & integrations**
//! remains an honest stub (no endpoint yet).

use crate::api;
use crate::components::SignInGate;
use crate::icons::{Ic, Icon};
use crate::state::use_session;
use dioxus::prelude::*;
use serde_json::Value;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Panel {
    Profile,
    Appearance,
    Security,
    Sync,
    Notifications,
}

impl Panel {
    const ALL: [Panel; 5] = [
        Self::Profile,
        Self::Appearance,
        Self::Security,
        Self::Sync,
        Self::Notifications,
    ];
    fn label(self) -> &'static str {
        match self {
            Self::Profile => "Profile",
            Self::Appearance => "Appearance",
            Self::Security => "Security & sessions",
            Self::Sync => "Sync & integrations",
            Self::Notifications => "Notification prefs",
        }
    }
}

#[component]
pub fn Account() -> Element {
    let session = use_session();
    let mut panel = use_signal(|| Panel::Profile);

    if !session.is_authenticated() {
        return rsx! {
            h1 { class: "ik-page-title", "Account" }
            SignInGate {}
        };
    }

    let name = session.username().unwrap_or_else(|| "reader".to_owned());
    let role = *session.role.read();
    let role_label = if role.is_admin() {
        "admin"
    } else if role.is_operator() {
        "operator"
    } else {
        "reader"
    };
    let current = *panel.read();

    rsx! {
        div { style: "display:flex;align-items:center;justify-content:space-between;gap:12px;",
            h1 { class: "ik-page-title", "Account" }
            button {
                class: "ik-btn",
                onclick: move |_| {
                    spawn(async move {
                        let _ = api::logout().await;
                        session.clear();
                    });
                },
                "Sign out"
            }
        }
        div { class: "ik-tabs",
            for p in Panel::ALL {
                button {
                    class: if current == p { "ik-tab active" } else { "ik-tab" },
                    onclick: move |_| panel.set(p),
                    "{p.label()}"
                }
            }
        }
        match current {
            Panel::Profile => rsx! { ProfilePanel { name: name.clone(), role: role_label } },
            Panel::Appearance => rsx! { AppearancePanel {} },
            Panel::Security => rsx! { SecurityPanel {} },
            Panel::Sync => rsx! {
                StubPanel { title: "Sync & integrations", note: "AniList connect + pull/push and conflict policy need the /v1/me/sync endpoints (not yet available)." }
            },
            Panel::Notifications => rsx! { NotificationsPanel {} },
        }
    }
}

#[component]
fn ProfilePanel(name: String, role: &'static str) -> Element {
    let session = use_session();
    let initial = name
        .chars()
        .next()
        .unwrap_or('?')
        .to_uppercase()
        .to_string();
    let mut username = use_signal(|| name.clone());
    let mut email = use_signal(String::new);
    let mut saving = use_signal(|| false);
    // `Ok(msg)` = success line, `Err(msg)` = error line, `None` = nothing said yet.
    let mut result: Signal<Option<Result<String, String>>> = use_signal(|| None);

    let save = move |_| {
        if *saving.peek() {
            return;
        }
        let new_username = username.peek().trim().to_owned();
        let new_email = email.peek().trim().to_owned();
        if new_username.is_empty() && new_email.is_empty() {
            result.set(Some(Err("Enter a new display name or email first.".to_owned())));
            return;
        }
        saving.set(true);
        result.set(None);
        spawn(async move {
            if let Some(t) = session.token_value() {
                let u = (!new_username.is_empty()).then_some(new_username.as_str());
                let e = (!new_email.is_empty()).then_some(new_email.as_str());
                match api::patch_profile(&t, u, e).await {
                    Ok(p) => {
                        username.set(p.username.clone());
                        result.set(Some(Ok("Saved. New sign-ins will reflect the change.".to_owned())));
                    }
                    Err(msg) => result.set(Some(Err(msg))),
                }
            }
            saving.set(false);
        });
    };

    let cur_name = username.read().clone();
    let msg = result.read().clone();
    rsx! {
        div { class: "ik-sidebar-card", style: "max-width:560px;",
            div { class: "ik-flex", style: "margin-bottom:16px;",
                div { class: "ik-avatar", style: "width:56px;height:56px;font-size:22px;", "{initial}" }
                div {
                    div { style: "font-family:var(--font-display);font-size:20px;font-weight:700;", "{cur_name}" }
                    div { class: "ik-mono ik-muted", style: "font-size:12px;", "{role}" }
                }
            }
            div { class: "ik-field",
                label { "Display name" }
                input {
                    class: "ik-input",
                    value: "{cur_name}",
                    oninput: move |e| username.set(e.value()),
                }
            }
            div { class: "ik-field",
                label { "Email" }
                input {
                    class: "ik-input",
                    r#type: "email",
                    placeholder: "new email address",
                    value: "{email}",
                    oninput: move |e| email.set(e.value()),
                }
            }
            match msg {
                Some(Ok(m)) => rsx! { p { class: "ik-muted", style: "font-size:13px;color:var(--jade,#3DA88F);", "{m}" } },
                Some(Err(m)) => rsx! { p { style: "font-size:13px;color:var(--acc);", "{m}" } },
                None => rsx! {},
            }
            button {
                class: "ik-btn primary",
                disabled: *saving.read(),
                onclick: save,
                if *saving.read() { "Saving…" } else { "Save profile" }
            }
        }
    }
}

/// Security & sessions (§9.4): list the caller's active login sessions and let them revoke
/// any one (its whole rotation family). 2FA/password change have no endpoint yet.
#[component]
fn SecurityPanel() -> Element {
    let session = use_session();
    let reload = use_signal(|| 0u32);
    let res = use_resource(move || {
        let _ = reload.read();
        async move {
            match session.token_value() {
                Some(t) => Some(api::sessions(&t).await),
                None => None,
            }
        }
    });

    let body = match &*res.read_unchecked() {
        None | Some(None) => rsx! { div { class: "ik-skeleton", style: "height:80px;" } },
        Some(Some(Err(e))) => rsx! {
            p { style: "font-size:13px;color:var(--acc);", "Could not load sessions: {e}" }
        },
        Some(Some(Ok(list))) if list.is_empty() => rsx! {
            div { class: "ik-empty", "No active sessions." }
        },
        Some(Some(Ok(list))) => {
            let rows = list.clone();
            rsx! {
                for s in rows {
                    SessionRow { key: "{s.id}", session_id: s.id.clone(), created_at: s.created_at.clone(), expires_at: s.expires_at.clone(), reload }
                }
            }
        }
    };

    rsx! {
        div { class: "ik-sidebar-card", style: "max-width:560px;",
            div { class: "ik-flex", style: "margin-bottom:12px;",
                Ic { icon: Icon::ShieldLock, size: 18 }
                strong { "Active sessions" }
            }
            p { class: "ik-muted", style: "font-size:13px;margin-top:0;", "Each device that signed in holds a refresh session. Revoke any you don't recognise." }
            {body}
            p { class: "ik-muted", style: "font-size:12px;margin-top:14px;", "Password change and two-factor auth aren't available yet." }
        }
    }
}

#[component]
fn SessionRow(
    session_id: String,
    created_at: String,
    expires_at: String,
    reload: Signal<u32>,
) -> Element {
    let session = use_session();
    let mut revoking = use_signal(|| false);
    let created = created_at.get(0..10).unwrap_or(&created_at).to_owned();
    let expires = expires_at.get(0..10).unwrap_or(&expires_at).to_owned();
    let revoke = move |_| {
        if *revoking.peek() {
            return;
        }
        revoking.set(true);
        let id = session_id.clone();
        let mut reload = reload;
        spawn(async move {
            if let Some(t) = session.token_value() {
                if api::delete_session(&t, &id).await.is_ok() {
                    reload += 1;
                }
            }
            revoking.set(false);
        });
    };
    rsx! {
        div { class: "ik-row",
            div { class: "grow",
                div { style: "font-weight:600;font-size:13px;", "Signed in {created}" }
                div { class: "ik-mono ik-muted", style: "font-size:11px;", "expires {expires}" }
            }
            button { class: "ik-btn", disabled: *revoking.read(), onclick: revoke,
                if *revoking.read() { "Revoking…" } else { "Revoke" }
            }
        }
    }
}

/// Which notification toggles the panel exposes. Stored as booleans in the open prefs JSON
/// document; an absent key defaults to enabled.
const NOTIFY_KEYS: [(&str, &str); 3] = [
    ("new_chapters", "New chapters in your watchlist"),
    ("email", "Email notifications"),
    ("digest", "Weekly digest"),
];

/// Notification preferences (§9.4): a set of on/off toggles persisted verbatim as the open
/// `notification_prefs` JSON document via `PUT /v1/me/notification-prefs`.
#[component]
fn NotificationsPanel() -> Element {
    let session = use_session();
    let mut prefs = use_signal(|| Value::Object(Default::default()));
    let mut loaded = use_signal(|| false);
    let mut saved = use_signal(|| false);

    use_effect(move || {
        spawn(async move {
            if let Some(t) = session.token_value() {
                if let Ok(v) = api::notification_prefs(&t).await {
                    prefs.set(v);
                }
            }
            loaded.set(true);
        });
    });

    if !*loaded.read() {
        return rsx! {
            div { class: "ik-sidebar-card", style: "max-width:560px;",
                div { class: "ik-skeleton", style: "height:80px;" }
            }
        };
    }

    let current = prefs.read().clone();
    rsx! {
        div { class: "ik-sidebar-card", style: "max-width:560px;",
            div { class: "ik-flex", style: "margin-bottom:12px;",
                Ic { icon: Icon::Notify, size: 18 }
                strong { "Notification preferences" }
            }
            for (key , label) in NOTIFY_KEYS {
                {
                    let on = current.get(key).and_then(Value::as_bool).unwrap_or(true);
                    rsx! {
                        div { class: "ik-row", key: "{key}",
                            span { class: "grow", "{label}" }
                            button {
                                class: if on { "ik-btn primary" } else { "ik-btn" },
                                onclick: move |_| {
                                    let mut v = prefs.read().clone();
                                    if !v.is_object() { v = Value::Object(Default::default()); }
                                    if let Some(obj) = v.as_object_mut() {
                                        obj.insert(key.to_owned(), Value::Bool(!on));
                                    }
                                    prefs.set(v.clone());
                                    saved.set(false);
                                    spawn(async move {
                                        if let Some(t) = session.token_value() {
                                            if api::set_notification_prefs(&t, &v).await.is_ok() {
                                                saved.set(true);
                                            }
                                        }
                                    });
                                },
                                if on { "On" } else { "Off" }
                            }
                        }
                    }
                }
            }
            if *saved.read() {
                p { class: "ik-muted", style: "font-size:13px;margin-top:10px;", "Preferences saved." }
            }
        }
    }
}

#[component]
fn AppearancePanel() -> Element {
    // Every knob is wired for real (DESIGN_SPEC §8): each writes a `data-*` attribute on
    // `<html>` and persists a `tv-*` key; `components.rs` re-applies them on boot. Non-default
    // values are stored; a default value clears the attribute/key so the :root default wins.
    let theme = use_signal(|| "dark".to_string());
    let accent = use_signal(|| "vermilion".to_string());
    let density = use_signal(|| "standard".to_string());
    let cover = use_signal(|| "ink".to_string());

    // Initialise each signal from the DOM/localStorage on mount.
    use_effect(move || {
        read_pref(theme, "tv-theme", "data-theme", "dark");
        read_pref(accent, "tv-accent", "data-accent", "vermilion");
        read_pref(density, "tv-density", "data-density", "standard");
        read_pref(cover, "tv-cover", "data-cover", "ink");
    });

    rsx! {
        div { class: "ik-sidebar-card", style: "max-width:560px;",
            div { class: "ik-flex", style: "margin-bottom:12px;",
                Ic { icon: Icon::Settings, size: 18 }
                strong { "Appearance" }
            }
            p { class: "ik-muted", style: "font-size:13px;margin-top:0;", "Tune the reading environment. Your choices are remembered on this device." }

            KnobGroup {
                title: "Theme",
                sig: theme,
                store_key: "tv-theme",
                attr: "data-theme",
                default: "dark",
                options: vec![("dark", "Inkstone Dark"), ("light", "Warm Paper")],
            }
            KnobGroup {
                title: "Accent",
                sig: accent,
                store_key: "tv-accent",
                attr: "data-accent",
                default: "vermilion",
                options: vec![
                    ("vermilion", "Vermilion"),
                    ("amber", "Amber"),
                    ("jade", "Jade"),
                    ("azure", "Azure"),
                    ("amethyst", "Amethyst"),
                ],
            }
            KnobGroup {
                title: "Density",
                sig: density,
                store_key: "tv-density",
                attr: "data-density",
                default: "standard",
                options: vec![("cozy", "Cozy"), ("standard", "Standard"), ("compact", "Compact")],
            }
            KnobGroup {
                title: "Cover style",
                sig: cover,
                store_key: "tv-cover",
                attr: "data-cover",
                default: "ink",
                options: vec![("ink", "Ink"), ("duotone", "Duotone"), ("vivid", "Vivid")],
            }
        }
    }
}

/// One labelled row of mutually-exclusive appearance chips.
#[component]
fn KnobGroup(
    title: &'static str,
    sig: Signal<String>,
    store_key: &'static str,
    attr: &'static str,
    default: &'static str,
    options: Vec<(&'static str, &'static str)>,
) -> Element {
    let cur = sig.read().clone();
    rsx! {
        div { style: "margin-top:16px;",
            div { class: "ik-subhead", style: "margin-bottom:8px;", "{title}" }
            div { class: "ik-chips", style: "margin-bottom:0;",
                for (value , label) in options {
                    button {
                        key: "{value}",
                        class: if cur == value { "ik-chip active" } else { "ik-chip" },
                        onclick: move |_| apply_pref(sig, attr, store_key, value, default),
                        "{label}"
                    }
                }
            }
        }
    }
}

/// Read a persisted appearance pref into `sig` (localStorage → current DOM attribute → default).
fn read_pref(
    mut sig: Signal<String>,
    key: &'static str,
    attr: &'static str,
    default: &'static str,
) {
    spawn(async move {
        if let Ok(v) = document::eval(&format!(
            "return (localStorage.getItem('{key}') || document.documentElement.getAttribute('{attr}') || '{default}');"
        ))
        .await
        {
            if let Some(s) = v.as_str() {
                sig.set(s.trim_matches('"').to_string());
            }
        }
    });
}

/// Apply and persist one appearance knob. A non-default value sets `data-{attr}` on `<html>`
/// and stores `tv-{key}`; selecting the default clears both so the `:root` default applies.
fn apply_pref(
    mut sig: Signal<String>,
    attr: &'static str,
    key: &'static str,
    value: &'static str,
    default: &'static str,
) {
    sig.set(value.to_string());
    // `data-theme` is always explicit (both dark/light are real values); the rest clear on default.
    let js = if value == default && attr != "data-theme" {
        format!(
            "document.documentElement.removeAttribute('{attr}');localStorage.removeItem('{key}');"
        )
    } else {
        format!("document.documentElement.setAttribute('{attr}','{value}');localStorage.setItem('{key}','{value}');")
    };
    let _ = document::eval(&js);
}

#[component]
fn StubPanel(title: &'static str, note: &'static str) -> Element {
    rsx! {
        div { class: "ik-sidebar-card", style: "max-width:560px;",
            strong { "{title}" }
            div { class: "ik-empty", style: "margin-top:12px;",
                p { "Not yet available." }
                p { class: "ik-muted", style: "font-size:13px;", "{note}" }
            }
        }
    }
}
