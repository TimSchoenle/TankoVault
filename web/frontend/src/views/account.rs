//! Account & settings (DESIGN_SPEC §7.7) — a settings shell with a sub-nav and panels.
//! **Profile** (`PATCH /v1/me/profile`), **Appearance** (`localStorage` + `data-*`),
//! **Security & sessions** (`GET`/`DELETE /v1/me/sessions`), **Notification prefs**
//! (`GET`/`PUT /v1/me/notification-prefs`) and **Sync & integrations** (`GET/DELETE
//! /v1/me/sync/anilist`, `POST /v1/me/sync/anilist/{pull,push}`) are all wired for real.

use crate::api;
use crate::components::{rel_time, SignInGate};
use crate::icons::{Ic, Icon};
use crate::models::ConflictPolicy;
use crate::state::use_session;
use crate::Route;
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
            Panel::Sync => rsx! { SyncPanel {} },
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

/// Sync & integrations: connect/disconnect `AniList`, pick a conflict policy, and run a pull
/// or push on demand. Status (`linked`, username, last sync) always comes from
/// `GET /v1/me/sync/anilist/status` — nothing here is claimed while actually unlinked.
#[component]
fn SyncPanel() -> Element {
    let session = use_session();
    let mut reload = use_signal(|| 0u32);
    let status = use_resource(move || {
        let _ = reload.read();
        async move {
            match session.token_value() {
                Some(t) => Some(api::anilist_status(&t).await),
                None => None,
            }
        }
    });
    let mut policy = use_signal(|| ConflictPolicy::NewestWins);
    let mut busy = use_signal(|| false);
    let mut message: Signal<Option<Result<String, String>>> = use_signal(|| None);

    let connect = move |_| {
        spawn(async move {
            if let Some(t) = session.token_value() {
                match api::anilist_authorize_url(&t).await {
                    Ok(url) => {
                        let js = format!(
                            "window.location.href = {};",
                            serde_json::to_string(&url).unwrap_or_default()
                        );
                        let _ = document::eval(&js);
                    }
                    Err(e) => message.set(Some(Err(e))),
                }
            }
        });
    };

    let disconnect = move |_| {
        if *busy.peek() {
            return;
        }
        busy.set(true);
        message.set(None);
        spawn(async move {
            if let Some(t) = session.token_value() {
                match api::anilist_disconnect(&t).await {
                    Ok(()) => {
                        message.set(Some(Ok("Disconnected from AniList.".to_owned())));
                        reload += 1;
                    }
                    Err(e) => message.set(Some(Err(e))),
                }
            }
            busy.set(false);
        });
    };

    let mut run = move |pull: bool| {
        if *busy.peek() {
            return;
        }
        busy.set(true);
        message.set(None);
        let policy = *policy.peek();
        spawn(async move {
            if let Some(t) = session.token_value() {
                let outcome = if pull {
                    api::anilist_pull(&t, policy).await
                } else {
                    api::anilist_push(&t, policy).await
                };
                match outcome {
                    Ok(v) => {
                        message.set(Some(Ok(summarize_sync(pull, &v))));
                        reload += 1;
                    }
                    Err(e) => message.set(Some(Err(e))),
                }
            }
            busy.set(false);
        });
    };

    let body = match &*status.read_unchecked() {
        None | Some(None) => rsx! { div { class: "ik-skeleton", style: "height:80px;" } },
        Some(Some(Err(e))) => rsx! {
            p { style: "font-size:13px;color:var(--acc);", "Could not load sync status: {e}" }
        },
        Some(Some(Ok(s))) if s.linked => {
            let username = s.username.clone().unwrap_or_else(|| "AniList reader".to_owned());
            let last_sync = rel_time(s.last_synced_at.as_deref());
            rsx! {
                div { class: "ik-flex", style: "gap:14px;margin-bottom:16px;",
                    div { class: "ik-source-tile", style: "width:46px;height:46px;",
                        Ic { icon: Icon::CloudDone, size: 22 }
                    }
                    div { class: "grow",
                        div { style: "font-weight:700;font-size:16px;", "AniList" }
                        div { class: "ik-flex", style: "gap:5px;font-size:13px;color:var(--jade,#3DA88F);",
                            Ic { icon: Icon::CloudDone, size: 15 }
                            "Connected as {username} · last sync {last_sync}"
                        }
                    }
                    button { class: "ik-btn", disabled: *busy.read(), onclick: disconnect, "Disconnect" }
                }
                div { class: "ik-subhead", style: "margin-bottom:8px;", "Conflict resolution policy" }
                div { class: "ik-chips",
                    for p in ConflictPolicy::ALL {
                        button {
                            key: "{p.label()}",
                            class: if *policy.read() == p { "ik-chip active" } else { "ik-chip" },
                            onclick: move |_| policy.set(p),
                            "{p.label()}"
                        }
                    }
                }
                div { class: "ik-flex", style: "gap:10px;flex-wrap:wrap;",
                    button {
                        class: "ik-btn",
                        disabled: *busy.read(),
                        onclick: move |_| run(true),
                        Ic { icon: Icon::CloudSync, size: 16 }
                        "Pull from AniList"
                    }
                    button {
                        class: "ik-btn",
                        disabled: *busy.read(),
                        onclick: move |_| run(false),
                        Ic { icon: Icon::CloudSync, size: 16 }
                        "Push to AniList"
                    }
                }
            }
        }
        Some(Some(Ok(_))) => rsx! {
            div { class: "ik-flex", style: "gap:14px;",
                div { class: "ik-source-tile", style: "width:46px;height:46px;",
                    Ic { icon: Icon::CloudOff, size: 22 }
                }
                div { class: "grow",
                    div { style: "font-weight:700;font-size:16px;", "AniList" }
                    div { class: "ik-muted", style: "font-size:13px;", "Not connected" }
                }
                button { class: "ik-btn primary", onclick: connect, "Connect AniList" }
            }
        },
    };

    rsx! {
        div { class: "ik-sidebar-card", style: "max-width:560px;",
            div { class: "ik-flex", style: "margin-bottom:14px;",
                Ic { icon: Icon::CloudDone, size: 18 }
                strong { "Sync & integrations" }
            }
            {body}
            match &*message.read() {
                Some(Ok(m)) => rsx! { p { style: "font-size:13px;color:var(--jade,#3DA88F);margin-top:12px;", "{m}" } },
                Some(Err(m)) => rsx! { p { style: "font-size:13px;color:var(--acc);margin-top:12px;", "{m}" } },
                None => rsx! {},
            }
        }
    }
}

/// One summary line from a pull's/push's report JSON (`engine::PullReport`/`PushReport`
/// shape, forwarded verbatim by the API).
fn summarize_sync(pull: bool, v: &Value) -> String {
    let n = |k: &str| v.get(k).and_then(Value::as_i64).unwrap_or(0);
    if pull {
        format!(
            "Pulled {} AniList entries — {} matched, {} updated, {} unmatched.",
            n("fetched"),
            n("matched"),
            n("updated"),
            n("unmatched")
        )
    } else {
        format!(
            "Pushed {} of {} watchlist entries to AniList ({} unmapped).",
            n("pushed"),
            n("considered"),
            n("unmapped")
        )
    }
}

/// Lands after the user approves (or declines) the `AniList` OAuth consent screen — the
/// `redirect_uri` configured on the sync service. That full-page browser round trip wipes the
/// SPA's in-memory session, so this waits for the boot-time silent refresh (`Shell`) to
/// restore the access token before calling the Bearer-authenticated link endpoint.
#[component]
pub fn AnilistCallback(code: String) -> Element {
    let session = use_session();
    let nav = use_navigator();
    let mut result: Signal<Option<Result<(), String>>> = use_signal(|| None);

    use_effect(move || {
        if !*session.ready.read() || result.peek().is_some() {
            return;
        }
        let code = code.clone();
        spawn(async move {
            let outcome = match session.token_value() {
                None => Err(
                    "Sign in, then connect AniList again from Account → Sync & integrations."
                        .to_owned(),
                ),
                Some(_) if code.trim().is_empty() => {
                    Err("AniList did not return an authorization code.".to_owned())
                }
                Some(t) => api::anilist_link(&t, &code).await,
            };
            let ok = outcome.is_ok();
            result.set(Some(outcome));
            if ok {
                nav.push(Route::Account {});
            }
        });
    });

    let elem = match &*result.read() {
        Some(Err(e)) => rsx! {
            div { class: "ik-empty",
                p { "Couldn't connect AniList: {e}" }
                Link { to: Route::Account {}, class: "ik-btn primary", "Back to Account" }
            }
        },
        _ => rsx! {
            div { class: "ik-empty", "Connecting to AniList…" }
        },
    };
    elem
}
