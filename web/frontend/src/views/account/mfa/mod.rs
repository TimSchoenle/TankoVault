//! Two-factor authentication on the Security panel: enrol an authenticator app or a security
//! key, keep recovery codes, and remove either.
//!
//! Rendered **above** the passkey card, because it is now the prerequisite for it: a passkey
//! signs in on its own with no second leg, so the account has to hold a second factor before it
//! may create one. A reader who arrives wanting a passkey should meet the thing they need first.
//!
//! Two rules shape this screen and neither is arbitrary:
//!
//! * **The secret is shown once.** `POST /v1/me/mfa/totp` is the only response that will ever
//!   carry it, and the same is true of a recovery-code set. So the card holds them in a signal
//!   until the reader dismisses them, and never re-fetches — there is nothing to re-fetch.
//! * **Every change needs an elevation once a factor exists.** The prompt is inline rather than
//!   a route of its own, so the list the reader is editing stays visible behind the question.

mod enrolment;

use enrolment::Mfa;

use crate::api;
use crate::components::{async_block, Field, InlineConfirm, PanelCard, StepUpGate, StepUpGuard};
use crate::hooks::{use_busy, Reload};
use crate::i18n::use_i18n;
use crate::icons::Icon;
use crate::state::step_up::use_step_up;
use crate::state::use_session;
use crate::util::iso_date;
use crate::webauthn::{self};
use crate::wire::types::SecurityKeyRename;
use dioxus::prelude::*;
use inkstone_ui::{Button, Tone};
use progenitor_client::ResponseValue;
#[component]
pub(crate) fn MfaCard() -> Element {
    let form = Mfa::use_new();
    let session = use_session();
    let mut removing_totp = use_signal(|| false);
    let Mfa {
        i18n,
        api,
        reload,
        busy,
        gate,
        error,
        info,
        pending_secret,
        mut confirm_code,
        mut fresh_codes,
        mut key_label,
        ..
    } = form;

    let status = use_resource(move || {
        reload.track();
        let client = api.client();
        let authed = session.is_authenticated();
        async move {
            if !authed {
                return Err(i18n.t("common.signInRequired"));
            }
            client
                .mfa_status()
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });

    let enrolled = status
        .read()
        .as_ref()
        .and_then(|r| r.as_ref().ok().map(|s| s.enrolled))
        .unwrap_or(false);

    let start_app_enrolment = use_callback(move |()| form.start_app_enrolment());
    let finish_app_enrolment = use_callback(move |()| form.finish_app_enrolment());
    let register_key = use_callback(move |()| form.register_key());
    let regenerate = use_callback(move |()| form.regenerate());
    let remove_totp = use_callback(move |()| form.remove_totp());

    let available = webauthn::is_available();

    rsx! {
        PanelCard { icon: Icon::ShieldLock, title: i18n.t("mfa.title"),
            p { class: "ik-muted", style: "font-size:13px;margin-top:0;", {i18n.t("mfa.intro")} }

            if let Some(msg) = error.read().clone() {
                div { class: "ik-error", style: "padding:10px;margin:10px 0;", "{msg}" }
            }
            if let Some(msg) = info.read().clone() {
                div { class: "ik-note", style: "padding:10px;margin:10px 0;", "{msg}" }
            }

            StepUpGuard { gate, enrolled }

            // The single sight of a fresh recovery-code set. Not dismissible by a stray click:
            // it is the only copy that will ever exist, and a reader who loses it has to
            // regenerate — which invalidates the set they were half-way through writing down.
            if !fresh_codes.read().is_empty() {
                div { class: "ik-note", style: "padding:12px;margin:12px 0;",
                    p { style: "margin:0 0 6px;font-weight:600;", {i18n.t("mfa.recovery.title")} }
                    p { class: "ik-muted", style: "font-size:13px;margin:0 0 8px;",
                        {i18n.t("mfa.recovery.warning")}
                    }
                    ul { style: "font-family:monospace;margin:0;padding-left:18px;",
                        for code in fresh_codes.read().iter().cloned() {
                            li { key: "{code}", "{code}" }
                        }
                    }
                    Button {
                        style: "margin-top:10px;",
                        on_click: move |_| fresh_codes.set(Vec::new()),
                        {i18n.t("mfa.recovery.saved")}
                    }
                }
            }

            // --- authenticator app ---
            h4 { style: "margin:16px 0 6px;font-size:14px;", {i18n.t("mfa.totp.title")} }
            {
                let s = status.read();
                let confirmed = s
                    .as_ref()
                    .and_then(|r| r.as_ref().ok())
                    .and_then(|s| s.totp_confirmed_at.clone());
                let can_totp = s
                    .as_ref()
                    .and_then(|r| r.as_ref().ok().map(|s| s.totp_available))
                    .unwrap_or(false);
                rsx! {
                    if let Some(at) = confirmed {
                        div { class: "ik-flex", style: "justify-content:space-between;align-items:center;gap:8px;",
                            span { class: "ik-muted", style: "font-size:13px;",
                                {i18n.t("mfa.totp.enrolledOn")} " " {iso_date(Some(&at))}
                            }
                            if *removing_totp.read() {
                                InlineConfirm {
                                    title: i18n.t("mfa.totp.remove.title"),
                                    body: i18n.t("mfa.totp.remove.body"),
                                    cta: i18n.t("common.remove"),
                                    busy: busy.is_busy(),
                                    on_cancel: move |()| removing_totp.set(false),
                                    on_confirm: move |()| {
                                        removing_totp.set(false);
                                        gate.attempt(move || remove_totp.call(()));
                                    },
                                }
                            } else {
                                Button {
                                    on_click: move |_| removing_totp.set(true),
                                    {i18n.t("common.remove")}
                                }
                            }
                        }
                    } else if !can_totp {
                        // The operator configured no sealing key, so enrolment would answer 503.
                        // A button that can only report that is worse than no button.
                        p { class: "ik-muted", style: "font-size:13px;",
                            {i18n.t("mfa.totp.unavailable")}
                        }
                    } else if let Some((secret, uri)) = pending_secret.read().clone() {
                        div {
                            p { class: "ik-muted", style: "font-size:13px;",
                                {i18n.t("mfa.totp.scan")}
                            }
                            QrCode { data: uri }
                            p { style: "font-family:monospace;font-size:13px;word-break:break-all;",
                                "{secret}"
                            }
                            Field {
                                id: "tv-totp-confirm",
                                label: i18n.t("mfa.totp.field.code"),
                                autocomplete: "one-time-code",
                                value: confirm_code(),
                                on_input: move |v| confirm_code.set(v),
                                on_enter: move |()| finish_app_enrolment.call(()),
                            }
                            Button {
                                tone: Tone::Primary,
                                disabled: busy.is_busy(),
                                on_click: move |_| finish_app_enrolment.call(()),
                                {i18n.t("mfa.totp.confirm")}
                            }
                        }
                    } else {
                        Button {
                            disabled: busy.is_busy(),
                            on_click: move |_| gate.attempt(move || start_app_enrolment.call(())),
                            {i18n.t("mfa.totp.add")}
                        }
                    }
                }
            }

            // --- security keys ---
            h4 { style: "margin:20px 0 6px;font-size:14px;", {i18n.t("mfa.key.title")} }
            {
                // `async_block`, not `async_list`: the keys are a field of the status document
                // rather than the whole response, so the empty case is rendered here.
                async_block(&status, reload, 48, |s| {
                    if s.security_keys.is_empty() {
                        return rsx! {
                            p { class: "ik-muted", style: "font-size:13px;",
                                {i18n.t("mfa.key.empty")}
                            }
                        };
                    }
                    rsx! {
                        for key in s.security_keys.iter().cloned() {
                            SecurityKeyRow {
                                key: "{key.id}",
                                id: key.id.to_string(),
                                label: key.label,
                                created_at: key.created_at,
                                last_used_at: key.last_used_at,
                                reload,
                                gate,
                            }
                        }
                    }
                })
            }

            if available {
                div { style: "margin-top:12px;",
                    Field {
                        id: "tv-security-key-label",
                        label: i18n.t("mfa.key.field.label"),
                        value: key_label(),
                        on_input: move |v| key_label.set(v),
                        on_enter: move |()| gate.attempt(move || register_key.call(())),
                    }
                    Button {
                        disabled: busy.is_busy(),
                        on_click: move |_| gate.attempt(move || register_key.call(())),
                        {i18n.t("mfa.key.add")}
                    }
                }
            } else {
                p { class: "ik-muted", style: "font-size:12px;margin-top:12px;",
                    {i18n.t("mfa.key.unsupported")}
                }
            }

            // --- recovery codes ---
            if enrolled {
                {
                    let remaining = status
                        .read()
                        .as_ref()
                        .and_then(|r| r.as_ref().ok().map(|s| s.recovery_codes_remaining))
                        .unwrap_or(0);
                    rsx! {
                        h4 { style: "margin:20px 0 6px;font-size:14px;",
                            {i18n.t("mfa.recovery.heading")}
                        }
                        p { class: "ik-muted", style: "font-size:13px;margin:0 0 8px;",
                            {i18n.t("mfa.recovery.remaining")} " " "{remaining}"
                        }
                        Button {
                            disabled: busy.is_busy(),
                            on_click: move |_| gate.attempt(move || regenerate.call(())),
                            {i18n.t("mfa.recovery.regenerate")}
                        }
                    }
                }
            }
        }
    }
}

/// One registered security key, with rename and revoke.
///
/// `gate` belongs to the card: both actions are elevated, and a refusal has to open the one
/// prompt above the list rather than a prompt per row.
#[component]
fn SecurityKeyRow(
    id: String,
    label: String,
    created_at: String,
    last_used_at: Option<String>,
    reload: Reload,
    gate: StepUpGate,
) -> Element {
    let i18n = use_i18n();
    let api = api::use_api();
    let step_up = use_step_up();
    let busy = use_busy();
    let mut error = use_signal(|| Option::<String>::None);
    let mut renaming = use_signal(|| false);
    let mut revoking = use_signal(|| false);
    let mut draft = use_signal(|| label.clone());

    let key_id = id.clone();
    let rename = use_callback(move |()| {
        if !busy.claim() {
            return;
        }
        let id = key_id.clone();
        let label = draft.read().trim().to_owned();
        let client = gate.client(api);
        spawn(async move {
            match client
                .rename_security_key()
                .id(id)
                .body(SecurityKeyRename { label })
                .send()
                .await
            {
                Ok(_) => {
                    renaming.set(false);
                    reload.bump();
                }
                Err(e) => {
                    if !gate.refused(api::Refusal::of(&e)) {
                        error.set(Some(api::friendly_error(i18n, e)));
                    }
                }
            }
            busy.release();
        });
    });

    let key_id = id.clone();
    let revoke = use_callback(move |()| {
        if !busy.claim() {
            return;
        }
        let id = key_id.clone();
        let client = gate.client(api);
        spawn(async move {
            match client.delete_security_key().id(id).send().await {
                Ok(_) => {
                    // Revoking a factor revokes every grant server-side; drop ours to match.
                    step_up.clear();
                    reload.bump();
                }
                Err(e) => {
                    if !gate.refused(api::Refusal::of(&e)) {
                        error.set(Some(api::friendly_error(i18n, e)));
                    }
                }
            }
            busy.release();
        });
    });

    rsx! {
        div { style: "padding:8px 0;border-top:1px solid var(--ik-line);",
            if let Some(msg) = error.read().clone() {
                div { class: "ik-error", style: "padding:6px;margin-bottom:6px;", "{msg}" }
            }
            div { class: "ik-flex", style: "justify-content:space-between;align-items:center;gap:8px;",
                if *renaming.read() {
                    Field {
                        id: "tv-key-rename-{id}",
                        label: i18n.t("mfa.key.field.label"),
                        value: draft(),
                        on_input: move |v| draft.set(v),
                        on_enter: move |()| gate.attempt(move || rename.call(())),
                    }
                } else {
                    div {
                        div { style: "font-weight:500;", "{label}" }
                        div { class: "ik-muted", style: "font-size:12px;",
                            {i18n.t("mfa.key.addedOn")} " " {iso_date(Some(&created_at))}
                            if let Some(used) = last_used_at.clone() {
                                " · " {i18n.t("mfa.key.lastUsed")} " " {iso_date(Some(&used))}
                            }
                        }
                    }
                }
                div { class: "ik-flex", style: "gap:6px;",
                    Button {
                        on_click: move |_| {
                            let open = *renaming.read();
                            if open {
                                gate.attempt(move || rename.call(()));
                            } else {
                                renaming.set(true);
                            }
                        },
                        if *renaming.read() {
                        {i18n.t("common.save")}
                        } else {
                        {i18n.t("common.rename")}
                        }
                    }
                    if *revoking.read() {
                        InlineConfirm {
                            title: i18n.t("mfa.key.revoke.title"),
                            body: i18n.t("mfa.key.revoke.body"),
                            cta: i18n.t("common.revoke"),
                            busy: busy.is_busy(),
                            on_cancel: move |()| revoking.set(false),
                            on_confirm: move |()| {
                                revoking.set(false);
                                gate.attempt(move || revoke.call(()));
                            },
                        }
                    } else {
                        Button {
                            on_click: move |_| revoking.set(true),
                            {i18n.t("common.revoke")}
                        }
                    }
                }
            }
        }
    }
}

/// The provisioning URI as a scannable QR code.
///
/// Drawn as `<rect>` elements from the module matrix rather than injected as markup: the served
/// CSP carries no `'unsafe-eval'` and this crate bans `document::eval` outright, and
/// `dangerous_inner_html` for something this component generates itself would be a habit worth
/// not forming. The matrix is small enough that the element count is unremarkable.
#[component]
fn QrCode(data: String) -> Element {
    let Ok(code) = qrcode::QrCode::new(data.as_bytes()) else {
        // An over-long URI cannot be encoded. The base32 secret is rendered beside this either
        // way, so the reader still has a way through — silence beats an error box here.
        return rsx! {};
    };
    let width = code.width();
    let colors = code.into_colors();
    let size = width + 8; // a four-module quiet zone, as the specification requires

    rsx! {
        svg {
            width: "180",
            height: "180",
            view_box: "0 0 {size} {size}",
            "shape-rendering": "crispEdges",
            rect { x: "0", y: "0", width: "{size}", height: "{size}", fill: "#ffffff" }
            for (i, module) in colors.iter().enumerate() {
                if *module == qrcode::Color::Dark {
                    rect {
                        key: "{i}",
                        x: "{i % width + 4}",
                        y: "{i / width + 4}",
                        width: "1",
                        height: "1",
                        fill: "#000000",
                    }
                }
            }
        }
    }
}
