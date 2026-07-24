//! Account & settings (DESIGN_SPEC §7.7) — a settings shell with a sub-nav and panels.
//! **Profile** (`PATCH /v1/me/profile`), **Appearance** (`localStorage` + `data-*`),
//! **Security & sessions** (`GET`/`DELETE /v1/me/sessions`), **Notification prefs**
//! (`GET`/`PUT /v1/me/notification-prefs`) and **Sync & integrations** (`GET/DELETE
//! /v1/me/sync/anilist`, `POST /v1/me/sync/anilist/{pull,push}`) are all wired for real.

use crate::api;
use crate::components::{rel_time, SignInGate};
use crate::icons::{Ic, Icon};
use crate::models::*;
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
    let api_client = api::use_api();

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
                    let client = api_client.clone();
                    spawn(async move {
                        let _ = client.logout().send().await;
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
    let api_client = api::use_api();
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
            result.set(Some(Err(
                "Enter a new display name or email first.".to_owned()
            )));
            return;
        }
        saving.set(true);
        result.set(None);
        let client = api_client.clone();
        spawn(async move {
            let update = ProfileUpdate {
                username: (!new_username.is_empty()).then_some(new_username),
                email: (!new_email.is_empty()).then_some(new_email),
            };
            match client.patch_profile().body(update).send().await {
                Ok(res) => {
                    let p = res.into_inner();
                    // Reflect the server's canonical values immediately — both in this
                    // form and, crucially, across the whole app (header, greeting, …) by
                    // overriding the session display name. No relog required.
                    username.set(p.username.clone());
                    session.set_display_name(p.username.clone());
                    email.set(String::new());
                    result.set(Some(Ok("Profile updated.".to_owned())));
                }
                Err(e) => result.set(Some(Err(api::friendly_error(e)))),
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
    let api_client = api::use_api();
    let res = use_resource(move || {
        let _ = reload.read();
        let client = api_client.clone();
        async move {
            if session.is_authenticated() {
                Some(
                    client
                        .sessions()
                        .send()
                        .await
                        .map(|r| r.into_inner())
                        .map_err(api::friendly_error),
                )
            } else {
                None
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
    let api_client = api::use_api();
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
        let client = api_client.clone();
        spawn(async move {
            if client.delete_session().id(id).send().await.is_ok() {
                reload += 1;
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
    let api_client = api::use_api();
    let mut prefs = use_signal(|| Value::Object(Default::default()));
    let mut loaded = use_signal(|| false);
    let mut saved = use_signal(|| false);

    {
        let client = api_client.clone();
        use_effect(move || {
            let client = client.clone();
            spawn(async move {
                if let Ok(res) = client.notification_prefs().send().await {
                    prefs.set(res.into_inner());
                }
                loaded.set(true);
            });
        });
    }

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
                    let client = api_client.clone();
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
                                    let client = client.clone();
                                    spawn(async move {
                                        if client.put_notification_prefs().body(v).send().await.is_ok() {
                                            saved.set(true);
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

/// Sync & integrations: one card per registered provider (design: generalized multi-provider
/// sync). `AniList` is the only provider registered today, but the panel is data-driven —
/// `GET /v1/me/sync/providers` — instead of a single hardcoded block.
#[component]
fn SyncPanel() -> Element {
    let session = use_session();
    let api_client = api::use_api();
    let providers = {
        let client = api_client.clone();
        use_resource(move || {
            let client = client.clone();
            async move {
                if session.is_authenticated() {
                    api::fetch_json(&client, "/v1/me/sync/providers")
                        .await
                        .and_then(|v| {
                            serde_json::from_value::<Vec<ProviderInfo>>(v)
                                .map_err(|e| format!("JSON error: {e}"))
                        })
                } else {
                    Ok(Vec::new())
                }
            }
        })
    };

    rsx! {
        div { class: "ik-sidebar-card", style: "max-width:560px;",
            div { class: "ik-flex", style: "margin-bottom:14px;",
                Ic { icon: Icon::CloudDone, size: 18 }
                strong { "Sync & integrations" }
            }
            match &*providers.read_unchecked() {
                None => rsx! { div { class: "ik-skeleton", style: "height:80px;" } },
                Some(Err(e)) => rsx! {
                    p { style: "font-size:13px;color:var(--acc);", "Could not load sync providers: {e}" }
                },
                Some(Ok(list)) if list.is_empty() => rsx! {
                    div { class: "ik-empty", "No sync providers are configured." }
                },
                Some(Ok(list)) => rsx! {
                    for p in list.clone() {
                        ProviderSyncCard {
                            key: "{p.slug_or_id()}",
                            slug: p.slug_or_id().to_owned(),
                            name: p.name.clone(),
                        }
                    }
                },
            }
        }
    }
}

/// One provider's connect/disconnect + pull/push card — the former single-AniList `SyncPanel`
/// body, parameterized by `slug`/`name`. Status (`linked`, username, last sync) always comes
/// from `GET /v1/me/sync/:provider/status` — nothing here is claimed while actually unlinked.
#[component]
fn ProviderSyncCard(slug: String, name: String) -> Element {
    let session = use_session();
    let mut reload = use_signal(|| 0u32);
    let api_client = api::use_api();
    let status = use_resource({
        let slug = slug.clone();
        let client = api_client.clone();
        move || {
            let _ = reload.read();
            let slug = slug.clone();
            let client = client.clone();
            async move {
                if session.is_authenticated() {
                    Some(
                        api::fetch_json(&client, &format!("/v1/me/sync/{slug}/status"))
                            .await
                            .and_then(|v| {
                                serde_json::from_value::<SyncStatus>(v).or(Ok(SyncStatus {
                                    linked: false,
                                    display_name: None,
                                    last_sync: None,
                                }))
                            }),
                    )
                } else {
                    None
                }
            }
        }
    });
    let mut policy = use_signal(|| ConflictPolicy::NewestWins);
    let mut auto_sync = use_signal(|| true);
    let mut show_conflicts = use_signal(|| false);
    let mut busy = use_signal(|| false);
    let mut message: Signal<Option<Result<String, String>>> = use_signal(|| None);

    // The account's persisted automatic-sync settings (design v2 §B.6/§B.8): drives the
    // "Automatic sync" toggle, the conflict-policy picker's initial value, and the pending
    // conflicts badge. Reloaded alongside `status` whenever `reload` bumps.
    let settings = use_resource({
        let slug = slug.clone();
        let client = api_client.clone();
        move || {
            let _ = reload.read();
            let slug = slug.clone();
            let client = client.clone();
            async move {
                if session.is_authenticated() {
                    Some(
                        api::fetch_json(&client, &format!("/v1/me/sync/{slug}/settings"))
                            .await
                            .and_then(|v| {
                                serde_json::from_value::<SyncSettings>(v).or(Ok(SyncSettings {
                                    auto_pull: true,
                                    auto_push: true,
                                    conflict_policy: "newest_wins".to_owned(),
                                    auto_sync_enabled: true,
                                    pending_conflicts: 0,
                                }))
                            }),
                    )
                } else {
                    None
                }
            }
        }
    });
    // Initialise the local controls from the server's persisted values once loaded.
    use_effect(move || {
        if let Some(Some(Ok(s))) = &*settings.read_unchecked() {
            policy.set(ConflictPolicy::parse(&s.conflict_policy));
            auto_sync.set(s.auto_sync_enabled);
        }
    });

    let toggle_auto = {
        let slug = slug.clone();
        let client = api_client.clone();
        move |_| {
            let next = !*auto_sync.peek();
            auto_sync.set(next);
            let slug = slug.clone();
            let client = client.clone();
            spawn(async move {
                let patch = SyncSettingsPatch {
                    auto_sync_enabled: Some(next),
                    conflict_policy: None,
                };
                let _ = client
                    .sync_settings_patch()
                    .provider(slug)
                    .body(patch)
                    .send()
                    .await;
                reload += 1;
            });
        }
    };

    let connect = {
        let slug = slug.clone();
        let client = api_client.clone();
        move |_| {
            let slug = slug.clone();
            let client = client.clone();
            spawn(async move {
                match api::fetch_json(&client, &format!("/v1/me/sync/{slug}/authorize")).await {
                    Ok(v) => {
                        let url = v
                            .as_str()
                            .map(str::to_owned)
                            .or_else(|| v.get("url").and_then(Value::as_str).map(str::to_owned))
                            .or_else(|| {
                                v.get("authorize_url")
                                    .and_then(Value::as_str)
                                    .map(str::to_owned)
                            });
                        match url {
                            Some(url) => {
                                let js = format!(
                                    "window.location.href = {};",
                                    serde_json::to_string(&url).unwrap_or_default()
                                );
                                let _ = document::eval(&js);
                            }
                            None => message.set(Some(Err(
                                "Sync provider did not return an authorize URL.".to_owned(),
                            ))),
                        }
                    }
                    Err(e) => message.set(Some(Err(e))),
                }
            });
        }
    };

    let disconnect = {
        let slug = slug.clone();
        let name = name.clone();
        let client = api_client.clone();
        move |_| {
            if *busy.peek() {
                return;
            }
            busy.set(true);
            message.set(None);
            let slug = slug.clone();
            let name = name.clone();
            let client = client.clone();
            spawn(async move {
                match client.sync_disconnect().provider(slug).send().await {
                    Ok(_) => {
                        message.set(Some(Ok(format!("Disconnected from {name}."))));
                        reload += 1;
                    }
                    Err(e) => message.set(Some(Err(api::friendly_error(e)))),
                }
                busy.set(false);
            });
        }
    };

    // Two independent closures (not one `run(bool)` parameterized closure): the latter would
    // capture non-`Copy` `slug` in its environment, and both buttons `move`-capturing the same
    // closure would double-move it.
    let pull_action = {
        let slug = slug.clone();
        let client = api_client.clone();
        move |_| {
            if *busy.peek() {
                return;
            }
            busy.set(true);
            message.set(None);
            let policy = *policy.peek();
            let slug = slug.clone();
            let client = client.clone();
            spawn(async move {
                let opts = SyncOpts {
                    policy: Some(policy.token().to_owned()),
                };
                match client
                    .sync_pull()
                    .provider(slug)
                    .body(SyncPullBody::Variant1(opts))
                    .send()
                    .await
                {
                    Ok(_) => {
                        message.set(Some(Ok("Sync pull started.".to_owned())));
                        reload += 1;
                    }
                    Err(e) => message.set(Some(Err(api::friendly_error(e)))),
                }
                busy.set(false);
            });
        }
    };

    let push_action = {
        let slug = slug.clone();
        let client = api_client.clone();
        move |_| {
            if *busy.peek() {
                return;
            }
            busy.set(true);
            message.set(None);
            let policy = *policy.peek();
            let slug = slug.clone();
            let client = client.clone();
            spawn(async move {
                let opts = SyncOpts {
                    policy: Some(policy.token().to_owned()),
                };
                match client
                    .sync_push()
                    .provider(slug)
                    .body(SyncPushBody::Variant1(opts))
                    .send()
                    .await
                {
                    Ok(_) => {
                        message.set(Some(Ok("Sync push started.".to_owned())));
                        reload += 1;
                    }
                    Err(e) => message.set(Some(Err(api::friendly_error(e)))),
                }
                busy.set(false);
            });
        }
    };

    let pending = match &*settings.read_unchecked() {
        Some(Some(Ok(s))) => s.pending_conflicts,
        _ => 0,
    };
    let linked = matches!(&*status.read_unchecked(), Some(Some(Ok(s))) if s.linked);

    let body = match &*status.read_unchecked() {
        None | Some(None) => rsx! { div { class: "ik-skeleton", style: "height:80px;" } },
        Some(Some(Err(e))) => rsx! {
            p { style: "font-size:13px;color:var(--acc);", "Could not load sync status: {e}" }
        },
        Some(Some(Ok(s))) if s.linked => {
            let username = s
                .display_name
                .clone()
                .unwrap_or_else(|| format!("{name} reader"));
            let last_sync = rel_time(s.last_sync.as_deref());
            rsx! {
                div { class: "ik-flex", style: "gap:14px;margin-bottom:16px;",
                    div { class: "ik-source-tile", style: "width:46px;height:46px;",
                        Ic { icon: Icon::CloudDone, size: 22 }
                    }
                    div { class: "grow",
                        div { style: "font-weight:700;font-size:16px;", "{name}" }
                        div { class: "ik-flex", style: "gap:5px;font-size:13px;color:var(--jade,#3DA88F);",
                            Ic { icon: Icon::CloudDone, size: 15 }
                            "Connected as {username} · last sync {last_sync}"
                        }
                    }
                    button { class: "ik-btn", disabled: *busy.read(), onclick: disconnect, "Disconnect" }
                }
                div { class: "ik-row", style: "margin-bottom:12px;",
                    div { class: "grow",
                        div { style: "font-weight:600;font-size:13px;", "Automatic sync" }
                        div { class: "ik-muted", style: "font-size:12px;", "Keep this account in sync in the background." }
                    }
                    button {
                        class: if *auto_sync.read() { "ik-btn primary" } else { "ik-btn" },
                        onclick: toggle_auto,
                        if *auto_sync.read() { "On" } else { "Off" }
                    }
                }
                if pending > 0 {
                    div { class: "ik-row", style: "margin-bottom:12px;",
                        span { class: "grow", style: "font-size:13px;color:var(--acc);",
                            "{pending} need your review"
                        }
                        button {
                            class: "ik-btn",
                            onclick: move |_| show_conflicts.set(true),
                            "Review conflicts"
                        }
                    }
                }
                div { class: "ik-subhead", style: "margin-bottom:8px;", "When local and AniList disagree" }
                div { class: "ik-chips",
                    for p in ConflictPolicy::ALL {
                        {
                            let slug = slug.clone();
                            let client = api_client.clone();
                            rsx! {
                                button {
                                    key: "{p.label()}",
                                    class: if *policy.read() == p { "ik-chip active" } else { "ik-chip" },
                                    onclick: move |_| {
                                        policy.set(p);
                                        let slug = slug.clone();
                                        let client = client.clone();
                                        spawn(async move {
                                            let patch = SyncSettingsPatch {
                                                auto_sync_enabled: None,
                                                conflict_policy: Some(p.token().to_owned()),
                                            };
                                            let _ = client
                                                .sync_settings_patch()
                                                .provider(slug)
                                                .body(patch)
                                                .send()
                                                .await;
                                            reload += 1;
                                        });
                                    },
                                    "{p.label()}"
                                }
                            }
                        }
                    }
                }
                div { class: "ik-flex", style: "gap:10px;flex-wrap:wrap;",
                    button {
                        class: "ik-btn",
                        disabled: *busy.read(),
                        onclick: pull_action,
                        Ic { icon: Icon::CloudSync, size: 16 }
                        "Pull from {name}"
                    }
                    button {
                        class: "ik-btn",
                        disabled: *busy.read(),
                        onclick: push_action,
                        Ic { icon: Icon::CloudSync, size: 16 }
                        "Push to {name}"
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
                    div { style: "font-weight:700;font-size:16px;", "{name}" }
                    div { class: "ik-muted", style: "font-size:13px;", "Not connected" }
                }
                button { class: "ik-btn primary", onclick: connect, "Connect {name}" }
            }
        },
    };

    rsx! {
        {body}
        match &*message.read() {
            Some(Ok(m)) => rsx! { p { style: "font-size:13px;color:var(--jade,#3DA88F);margin-top:12px;", "{m}" } },
            Some(Err(m)) => rsx! { p { style: "font-size:13px;color:var(--acc);margin-top:12px;", "{m}" } },
            None => rsx! {},
        }
        if *show_conflicts.read() {
            ConflictInbox { provider: slug.clone(), show: show_conflicts, parent_reload: reload }
        }
        if linked {
            SyncHistory { provider: slug.clone(), refresh: reload }
        }
    }
}

/// A compact "recent sync activity" log for one provider (design v2 §B.4/§B.6): what the
/// automatic engine actually did, so "automatic" never means "invisible." Reloads whenever
/// the parent card's `refresh` bumps (after a pull/push/settings change).
#[component]
fn SyncHistory(provider: String, refresh: Signal<u32>) -> Element {
    let session = use_session();
    let api_client = api::use_api();
    let prov = provider.clone();
    let entries = use_resource(move || {
        let _ = refresh.read();
        let prov = prov.clone();
        let client = api_client.clone();
        async move {
            if session.is_authenticated() {
                match api::fetch_json(&client, "/v1/me/sync/history").await {
                    Ok(v) => {
                        let list: Vec<Value> = v.as_array().cloned().unwrap_or_default();
                        Some(
                            list.into_iter()
                                .filter(|e| {
                                    e.get("provider").and_then(Value::as_str) == Some(&prov)
                                })
                                .take(8)
                                .collect::<Vec<_>>(),
                        )
                    }
                    Err(_) => None,
                }
            } else {
                None
            }
        }
    });

    let body = match &*entries.read_unchecked() {
        None => rsx! {},
        Some(None) => rsx! {},
        Some(Some(list)) if list.is_empty() => rsx! {
            div { class: "ik-muted", style: "font-size:13px;", "No automatic sync activity yet." }
        },
        Some(Some(list)) => {
            let rows = list.clone();
            rsx! {
                for e in rows {
                    {
                        let id = e.get("id").and_then(Value::as_str).unwrap_or("?");
                        let title = e.get("series_title").and_then(Value::as_str).unwrap_or("Unknown Series");
                        let action = e.get("action").and_then(Value::as_str).unwrap_or("sync");
                        let detail = e.get("field").and_then(Value::as_str).unwrap_or("progress");
                        let time = rel_time(e.get("created_at").and_then(Value::as_str));
                        rsx! {
                            div { class: "ik-row", key: "{id}",
                                div { class: "grow",
                                    div { style: "font-weight:600;font-size:13px;", "{title}" }
                                    div { class: "ik-mono ik-muted", style: "font-size:11px;",
                                        "{action} {detail} · {time}"
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    };

    rsx! {
        div { class: "ik-sidebar-card", style: "max-width:560px;margin-top:14px;",
            div { class: "ik-flex", style: "margin-bottom:12px;",
                Ic { icon: Icon::CloudSync, size: 16 }
                strong { "Recent sync activity" }
            }
            {body}
        }
    }
}

/// The user-facing conflict-resolution inbox (design v2 §B.8): every pending `sync_conflict`
/// for `provider`, each with a plain-language "keep mine / take AniList's" choice. Reused
/// interaction shape as the operator merge-candidates queue, for a reader audience.
#[component]
fn ConflictInbox(provider: String, show: Signal<bool>, parent_reload: Signal<u32>) -> Element {
    let session = use_session();
    let api_client = api::use_api();
    let reload = use_signal(|| 0u32);
    let list = use_resource(move || {
        let _ = reload.read();
        let client = api_client.clone();
        async move {
            if session.is_authenticated() {
                match api::fetch_json(&client, "/v1/me/sync/conflicts").await {
                    Ok(v) => serde_json::from_value::<Vec<SyncConflict>>(v)
                        .map_err(|e| format!("JSON error: {e}")),
                    Err(e) => Err(e),
                }
            } else {
                Ok(Vec::new())
            }
        }
    });

    let prov = provider.clone();
    let body = match &*list.read_unchecked() {
        None => rsx! { div { class: "ik-skeleton", style: "height:60px;" } },
        Some(Ok(all_vec)) => {
            let rows: Vec<SyncConflict> = all_vec
                .iter()
                .filter(|c| c.provider == prov)
                .cloned()
                .collect();
            if rows.is_empty() {
                rsx! { div { class: "ik-empty", "No conflicts need your review." } }
            } else {
                rsx! {
                    for c in rows {
                        ConflictRow { key: "{c.id}", conflict: Signal::new(c), reload, parent_reload }
                    }
                }
            }
        }
        Some(Err(e)) => rsx! {
            p { style: "font-size:13px;color:var(--acc);", "Could not load conflicts: {e}" }
        },
    };

    rsx! {
        div { class: "ik-sidebar-card", style: "max-width:560px;margin-top:14px;",
            div { class: "ik-flex", style: "margin-bottom:12px;",
                strong { class: "grow", "Conflicts to review" }
                button { class: "ik-btn", onclick: move |_| show.set(false), "Close" }
            }
            {body}
        }
    }
}

/// One conflict row: shows the disagreeing values and the two resolution buttons.
#[component]
fn ConflictRow(
    conflict: Signal<SyncConflict>,
    reload: Signal<u32>,
    parent_reload: Signal<u32>,
) -> Element {
    let api_client = api::use_api();
    let busy = use_signal(|| false);
    let con = conflict.read();

    // Each button gets its own closure with its own `id` clone (a shared `FnMut` capturing the
    // non-`Copy` `id` can't be moved into both buttons).
    let resolve_local = {
        let id = con.id.clone();
        let client = api_client.clone();
        move |_| {
            if *busy.peek() {
                return;
            }
            resolve_conflict(
                client.clone(),
                busy,
                reload,
                parent_reload,
                id.clone(),
                "local",
            );
        }
    };
    let resolve_remote = {
        let id = con.id.clone();
        let client = api_client.clone();
        move |_| {
            if *busy.peek() {
                return;
            }
            resolve_conflict(
                client.clone(),
                busy,
                reload,
                parent_reload,
                id.clone(),
                "remote",
            );
        }
    };

    rsx! {
        div { class: "ik-row",
            div { class: "grow",
                div { style: "font-weight:600;font-size:13px;", "{con.series_title}" }
                div { class: "ik-mono ik-muted", style: "font-size:11px;",
                    "{con.field}: local {con.local_value} · AniList {con.remote_value}"
                }
            }
            button {
                class: "ik-btn",
                disabled: *busy.read(),
                onclick: resolve_local,
                "Keep mine"
            }
            button {
                class: "ik-btn",
                disabled: *busy.read(),
                onclick: resolve_remote,
                "Take AniList's"
            }
        }
    }
}

/// Fire a conflict resolution and refresh both the inbox and the parent card's badge.
fn resolve_conflict(
    api_client: tankovault_api_client::Client,
    mut busy: Signal<bool>,
    mut reload: Signal<u32>,
    mut parent_reload: Signal<u32>,
    conflict_id: String,
    resolution: &'static str,
) {
    busy.set(true);
    spawn(async move {
        let body = ResolveConflict {
            resolution: resolution.to_owned(),
        };
        if let Ok(id) = uuid::Uuid::parse_str(&conflict_id) {
            if api_client
                .sync_resolve_conflict()
                .id(id)
                .body(body)
                .send()
                .await
                .is_ok()
            {
                reload += 1;
                parent_reload += 1;
            }
        }
        busy.set(false);
    });
}

/// Lands after the user approves (or declines) the `AniList` OAuth consent screen — the
/// `redirect_uri` configured on the sync service. That full-page browser round trip wipes the
/// SPA's in-memory session, so this waits for the boot-time silent refresh (`Shell`) to
/// restore the access token before calling the Bearer-authenticated link endpoint.
#[component]
pub fn AnilistCallback(code: String) -> Element {
    let session = use_session();
    let nav = use_navigator();
    let api_client = api::use_api();
    let mut result: Signal<Option<Result<(), String>>> = use_signal(|| None);

    use_effect(move || {
        if !*session.ready.read() || result.peek().is_some() {
            return;
        }
        let code = code.clone();
        let client = api_client.clone();
        spawn(async move {
            let outcome = match session.token_value() {
                None => Err(
                    "Sign in, then connect AniList again from Account → Sync & integrations."
                        .to_owned(),
                ),
                Some(_) if code.trim().is_empty() => {
                    Err("AniList did not return an authorization code.".to_owned())
                }
                Some(_) => client
                    .sync_callback()
                    .provider("anilist")
                    .code(code)
                    .send()
                    .await
                    .map(|_| ())
                    .map_err(api::friendly_error),
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
