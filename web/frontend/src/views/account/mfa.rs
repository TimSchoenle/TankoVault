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

use crate::api;
use crate::components::{
    async_block, use_step_up_gate, Field, InlineConfirm, PanelCard, StepUpGate, StepUpPrompt,
};
use crate::hooks::{use_busy, use_reload, Reload};
use crate::i18n::use_i18n;
use crate::icons::Icon;
use crate::state::step_up::use_step_up;
use crate::state::use_session;
use crate::util::iso_date;
use crate::webauthn::{self, CeremonyError};
use crate::wire::types::{
    SecurityKeyRegisterFinish, SecurityKeyRegisterStart, SecurityKeyRename, TotpConfirm,
};
use dioxus::prelude::*;
use progenitor_client::ResponseValue;
use webauthn_rs_proto::CreationChallengeResponse;

#[component]
pub(crate) fn MfaCard() -> Element {
    let session = use_session();
    let i18n = use_i18n();
    let api = api::use_api();
    let reload = use_reload();
    let busy = use_busy();
    // Both: the gate drives the prompt, and removing a factor has to drop the grant that factor
    // earned — the server revokes it, so keeping it here would only mean retrying with a dead
    // token.
    let step_up = use_step_up();
    let gate = use_step_up_gate();

    let mut error = use_signal(|| Option::<String>::None);
    let mut info = use_signal(|| Option::<String>::None);
    // The one and only sight of a freshly issued secret.
    let mut pending_secret = use_signal(|| Option::<(String, String)>::None);
    let mut confirm_code = use_signal(String::new);
    // The one and only sight of a recovery-code set.
    let mut fresh_codes = use_signal(Vec::<String>::new);
    let mut key_label = use_signal(String::new);
    let mut removing_totp = use_signal(|| false);

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

    // Every write below funnels through this, so the "a 403 means prompt, not fail" rule is
    // written once. A handler that reported the raw problem instead would show the reader
    // "insufficient privileges" for an action they are perfectly entitled to take.
    //
    // Takes the *classified* refusal and the message rather than the error itself: every
    // generated operation has its own error type, so one callback cannot accept them all — and
    // one callback is the point.
    let handle_refusal = use_callback(move |(refused, message): (api::Refusal, String)| {
        if gate.refused(refused) {
            error.set(None);
        } else {
            error.set(Some(message));
        }
    });

    let begin_totp = use_callback(move |()| {
        if !busy.claim() {
            return;
        }
        error.set(None);
        info.set(None);
        let client = gate.client(api);
        spawn(async move {
            match client.begin_totp().send().await {
                Ok(res) => {
                    let issued = ResponseValue::into_inner(res);
                    pending_secret.set(Some((issued.secret, issued.provisioning_uri)));
                }
                Err(e) => handle_refusal.call(refusal(i18n, e)),
            }
            busy.release();
        });
    });

    let confirm_totp = use_callback(move |()| {
        if !busy.claim() {
            return;
        }
        error.set(None);
        let code = confirm_code.read().trim().to_owned();
        let client = api.client();
        spawn(async move {
            match client
                .confirm_totp()
                .body(TotpConfirm { code })
                .send()
                .await
            {
                Ok(res) => {
                    fresh_codes.set(ResponseValue::into_inner(res).codes);
                    pending_secret.set(None);
                    confirm_code.set(String::new());
                    info.set(Some(i18n.t("mfa.totp.enrolled")));
                    reload.bump();
                }
                Err(e) => error.set(Some(match api::error_status(&e) {
                    Some(401) => i18n.t("mfa.error.wrongCode"),
                    _ => api::friendly_error(i18n, e),
                })),
            }
            busy.release();
        });
    });

    let register_key = use_callback(move |()| {
        if !busy.claim() {
            return;
        }
        error.set(None);
        info.set(None);
        let label = key_label.read().trim().to_owned();
        let client = gate.client(api);
        // The finish leg needs no elevation — the ceremony it completes was already gated — so
        // it goes through the ordinary client.
        let plain = api.client();

        spawn(async move {
            let started = match client
                .security_key_register_start()
                .body(SecurityKeyRegisterStart {
                    label: if label.is_empty() { None } else { Some(label) },
                })
                .send()
                .await
            {
                Ok(res) => ResponseValue::into_inner(res),
                Err(e) => {
                    handle_refusal.call(refusal(i18n, e));
                    busy.release();
                    return;
                }
            };

            // The authenticator. Everything from here can end in a shrug — a reader who walked
            // away from the prompt has changed nothing, and the card must look like it.
            let challenge: CreationChallengeResponse =
                match webauthn::parse_challenge(started.options) {
                    Ok(challenge) => challenge,
                    Err(e) => return ceremony_failed(&e, error, busy, i18n),
                };
            let credential = match webauthn::create(challenge).await {
                Ok(credential) => credential,
                Err(e) => return ceremony_failed(&e, error, busy, i18n),
            };
            let envelope = match webauthn::to_envelope(&credential) {
                Ok(envelope) => envelope,
                Err(e) => return ceremony_failed(&e, error, busy, i18n),
            };

            match plain
                .security_key_register_finish()
                .body(SecurityKeyRegisterFinish {
                    ceremony_id: started.ceremony_id,
                    credential: envelope,
                })
                .send()
                .await
            {
                Ok(res) => {
                    let registered = ResponseValue::into_inner(res);
                    // Present only when this was the account's *first* factor. Shown here or
                    // nowhere: the API will not send them again.
                    if let Some(codes) = registered.recovery_codes {
                        fresh_codes.set(codes);
                    }
                    key_label.set(String::new());
                    info.set(Some(i18n.t("mfa.key.added")));
                    reload.bump();
                }
                Err(e) => error.set(Some(match api::error_status(&e) {
                    Some(409) => i18n.t("mfa.error.alreadyRegistered"),
                    _ => api::friendly_error(i18n, e),
                })),
            }
            busy.release();
        });
    });

    let regenerate = use_callback(move |()| {
        if !busy.claim() {
            return;
        }
        error.set(None);
        let client = gate.client(api);
        spawn(async move {
            match client.regenerate_recovery_codes().send().await {
                Ok(res) => {
                    fresh_codes.set(ResponseValue::into_inner(res).codes);
                    reload.bump();
                }
                Err(e) => handle_refusal.call(refusal(i18n, e)),
            }
            busy.release();
        });
    });

    let remove_totp = use_callback(move |()| {
        if !busy.claim() {
            return;
        }
        error.set(None);
        let client = gate.client(api);
        spawn(async move {
            match client.delete_totp().send().await {
                Ok(_) => {
                    // The grant that authorised this was revoked with the factor, server-side.
                    // Dropping it here too keeps the client from retrying with a dead token.
                    step_up.clear();
                    info.set(Some(i18n.t("mfa.totp.removed")));
                    reload.bump();
                }
                Err(e) => handle_refusal.call(refusal(i18n, e)),
            }
            busy.release();
        });
    });

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

            if gate.is_open() {
                StepUpPrompt {
                    enrolled,
                    on_done: move |()| {
                        gate.close();
                        info.set(Some(i18n.t("stepUp.confirmed")));
                    },
                }
            }

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
                    button {
                        class: "ik-btn",
                        style: "margin-top:10px;",
                        onclick: move |_| fresh_codes.set(Vec::new()),
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
                                        remove_totp.call(());
                                    },
                                }
                            } else {
                                button {
                                    class: "ik-btn",
                                    onclick: move |_| removing_totp.set(true),
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
                                on_enter: move |()| confirm_totp.call(()),
                            }
                            button {
                                class: "ik-btn primary",
                                disabled: busy.is_busy(),
                                onclick: move |_| confirm_totp.call(()),
                                {i18n.t("mfa.totp.confirm")}
                            }
                        }
                    } else {
                        button {
                            class: "ik-btn",
                            disabled: busy.is_busy(),
                            onclick: move |_| begin_totp.call(()),
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
                        on_enter: move |()| register_key.call(()),
                    }
                    button {
                        class: "ik-btn",
                        disabled: busy.is_busy(),
                        onclick: move |_| register_key.call(()),
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
                        button {
                            class: "ik-btn",
                            disabled: busy.is_busy(),
                            onclick: move |_| regenerate.call(()),
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
                        on_enter: move |()| rename.call(()),
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
                    button {
                        class: "ik-btn",
                        onclick: move |_| {
                            let open = *renaming.read();
                            if open {
                                rename.call(());
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
                                revoke.call(());
                            },
                        }
                    } else {
                        button {
                            class: "ik-btn",
                            onclick: move |_| revoking.set(true),
                            {i18n.t("common.revoke")}
                        }
                    }
                }
            }
        }
    }
}

fn refusal(
    i18n: crate::i18n::Translator,
    err: progenitor_client::Error<crate::wire::types::ProblemDetails>,
) -> (api::Refusal, String) {
    (api::Refusal::of(&err), api::friendly_error(i18n, err))
}

/// Report a ceremony outcome and release the busy latch.
///
/// [`CeremonyError::Cancelled`] clears the error line rather than writing to it, for the reason
/// the passkey card gives: a ceremony the reader chose to stop is not a broken feature.
fn ceremony_failed(
    outcome: &CeremonyError,
    mut error: Signal<Option<String>>,
    busy: crate::hooks::Busy,
    i18n: crate::i18n::Translator,
) {
    if matches!(outcome, CeremonyError::Cancelled) {
        error.set(None);
    } else {
        error.set(Some(i18n.t(outcome.key())));
    }
    busy.release();
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
