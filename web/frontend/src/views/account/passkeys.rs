//! Passkeys on the Security panel: register one, rename it, revoke it.
//!
//! The browser half of the ceremony lives in [`crate::webauthn`]; this module is the screen
//! around it. Its shape is dictated by one fact: registering a passkey is *three* steps that
//! must all succeed — ask the API for a challenge, hand it to the authenticator, send the
//! result back — and the middle one can fail because a human walked away. So the card holds no
//! partial state between them, and a cancelled prompt leaves the list exactly as it was.

use crate::api;
use crate::components::{async_list, Field, InlineConfirm, PanelCard, SkeletonBlock};
use crate::hooks::{use_busy, use_reload, Reload};
use crate::i18n::use_i18n;
use crate::icons::Icon;
use crate::state::use_session;
use crate::util::iso_date;
use crate::webauthn::{self, CeremonyError};
use crate::wire::types::{PasskeyRegisterFinish, PasskeyRegisterStart, PasskeyRename};
use dioxus::prelude::*;
use progenitor_client::ResponseValue;
use webauthn_rs_proto::CreationChallengeResponse;

#[component]
pub(crate) fn PasskeysCard() -> Element {
    let session = use_session();
    let i18n = use_i18n();
    let api = api::use_api();
    let reload = use_reload();
    let busy = use_busy();

    let mut password = use_signal(String::new);
    let mut label = use_signal(String::new);
    let mut error = use_signal(|| Option::<String>::None);
    let mut info = use_signal(|| Option::<String>::None);
    // Whether the "add a passkey" form is open. Collapsed by default: it asks for a password,
    // and a password field sitting open on a page nobody came here to authenticate on invites
    // browsers to autofill it into a request the reader never made.
    let mut adding = use_signal(|| false);

    let passkeys = use_resource(move || {
        reload.track();
        let client = api.client();
        let authed = session.is_authenticated();
        async move {
            if !authed {
                return Ok(Vec::new());
            }
            client
                .list_passkeys()
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });

    let register = use_callback(move |()| {
        if !busy.claim() {
            return;
        }
        error.set(None);
        info.set(None);
        let password_v = password.read().clone();
        let label_v = label.read().trim().to_owned();
        let client = api.client();

        spawn(async move {
            // Leg 1: the API mints a challenge, after checking the password.
            let started = client
                .passkey_register_start()
                .body(PasskeyRegisterStart {
                    current_password: password_v,
                    label: if label_v.is_empty() {
                        None
                    } else {
                        Some(label_v)
                    },
                })
                .send()
                .await;
            let started = match started {
                Ok(res) => res.into_inner(),
                Err(e) => {
                    // A `401` here means the password was wrong, not that the session expired —
                    // the request carried a valid bearer token or it would not have reached the
                    // handler. The shared catalogue's "you need to sign in" would send the
                    // reader looking for a session problem that does not exist.
                    error.set(Some(match api::error_status(&e) {
                        Some(401) => i18n.t("passkey.error.badPassword"),
                        _ => api::friendly_error(i18n, e),
                    }));
                    busy.release();
                    return;
                }
            };

            // Leg 2: the authenticator. Everything from here on can end in a shrug.
            let challenge: CreationChallengeResponse =
                match webauthn::parse_challenge(started.options) {
                    Ok(challenge) => challenge,
                    Err(e) => return finish_with(&e, error, busy, i18n),
                };
            let credential = match webauthn::create(challenge).await {
                Ok(credential) => credential,
                Err(e) => return finish_with(&e, error, busy, i18n),
            };
            let envelope = match webauthn::to_envelope(&credential) {
                Ok(envelope) => envelope,
                Err(e) => return finish_with(&e, error, busy, i18n),
            };

            // Leg 3: the API verifies the attestation and stores the credential.
            match client
                .passkey_register_finish()
                .body(PasskeyRegisterFinish {
                    ceremony_id: started.ceremony_id,
                    credential: envelope,
                })
                .send()
                .await
            {
                Ok(_) => {
                    password.set(String::new());
                    label.set(String::new());
                    adding.set(false);
                    info.set(Some(i18n.t("passkey.added")));
                    reload.bump();
                }
                Err(e) => error.set(Some(match api::error_status(&e) {
                    Some(409) => i18n.t("passkey.error.alreadyRegistered"),
                    _ => api::friendly_error(i18n, e),
                })),
            }
            busy.release();
        });
    });

    // Hidden entirely where no ceremony can run — an "Add a passkey" button that can only ever
    // report "not available here" is worse than no button. Plain-HTTP development lands here too
    // on the web build, since `navigator.credentials` needs a secure context; on desktop it is
    // Windows Hello's presence, and every non-Windows desktop build.
    let available = webauthn::is_available();

    rsx! {
        PanelCard { icon: Icon::ShieldLock, title: i18n.t("passkey.title"),
            p { class: "ik-muted", style: "font-size:13px;margin-top:0;",
                {i18n.t("passkey.intro")}
            }

            if let Some(msg) = error.read().clone() {
                div { class: "ik-error", style: "padding:10px;margin:10px 0;", "{msg}" }
            }
            if let Some(msg) = info.read().clone() {
                div { class: "ik-note", style: "padding:10px;margin:10px 0;", "{msg}" }
            }

            {
                async_list(
                    &passkeys,
                    reload,
                    || rsx! { SkeletonBlock { height: 64 } },
                    &i18n.t("passkey.empty"),
                    |rows| rsx! {
                        for row in rows.iter().cloned() {
                            PasskeyRow {
                                key: "{row.id}",
                                id: row.id.to_string(),
                                label: row.label,
                                created_at: row.created_at,
                                last_used_at: row.last_used_at,
                                reload,
                            }
                        }
                    },
                )
            }

            if !available {
                p { class: "ik-muted", style: "font-size:12px;margin-top:14px;",
                    {i18n.t("passkey.error.unsupported")}
                }
            } else if *adding.read() {
                div { style: "margin-top:14px;",
                    Field {
                        id: "tv-passkey-label",
                        label: i18n.t("passkey.field.label"),
                        value: label(),
                        on_input: move |v| label.set(v),
                        on_enter: move |()| register.call(()),
                    }
                    Field {
                        id: "tv-passkey-password",
                        label: i18n.t("passkey.field.password"),
                        kind: "password",
                        autocomplete: "current-password",
                        value: password(),
                        on_input: move |v| password.set(v),
                        on_enter: move |()| register.call(()),
                        hint: i18n.t("passkey.whyPassword"),
                    }
                    div { class: "ik-flex", style: "gap:6px;",
                        button {
                            class: "ik-btn primary",
                            disabled: busy.is_busy(),
                            onclick: move |_| register.call(()),
                            if busy.is_busy() {
                                {i18n.t("common.working")}
                            } else {
                                {i18n.t("passkey.add")}
                            }
                        }
                        button {
                            class: "ik-btn",
                            r#type: "button",
                            onclick: move |_| {
                                adding.set(false);
                                password.set(String::new());
                                error.set(None);
                            },
                            {i18n.t("common.cancel")}
                        }
                    }
                }
            } else {
                button {
                    class: "ik-btn",
                    style: "margin-top:14px;",
                    r#type: "button",
                    onclick: move |_| {
                        error.set(None);
                        info.set(None);
                        adding.set(true);
                    },
                    {i18n.t("passkey.add")}
                }
            }
        }
    }
}

/// Report a ceremony outcome and release the busy latch.
///
/// [`CeremonyError::Cancelled`] clears the error line instead of writing to it — a ceremony the
/// reader chose to stop isn't a broken feature.
fn finish_with(
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

#[component]
fn PasskeyRow(
    id: String,
    label: String,
    created_at: String,
    last_used_at: Option<String>,
    reload: Reload,
) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let busy = use_busy();
    let mut renaming = use_signal(|| false);
    let mut confirming = use_signal(|| false);
    let mut draft = use_signal(|| label.clone());

    let created = iso_date(Some(&created_at)).to_owned();
    let used = last_used_at.as_deref().map_or_else(
        || i18n.t("passkey.neverUsed"),
        |t| i18n.args("passkey.lastUsed", &[("date", iso_date(Some(t)))]),
    );

    let revoke_id = id.clone();
    // `use_callback` rather than a plain closure: both of these are called from two places (the
    // button and the Enter key / the confirm dialog), and a bare closure is not `Copy`.
    let revoke = use_callback(move |()| {
        if !busy.claim() {
            return;
        }
        let id = revoke_id.clone();
        let client = api.client();
        spawn(async move {
            if client.delete_passkey().id(id).send().await.is_ok() {
                confirming.set(false);
                reload.bump();
            }
            busy.release();
        });
    });

    let rename_id = id.clone();
    let save = use_callback(move |()| {
        if !busy.claim() {
            return;
        }
        let id = rename_id.clone();
        let label = draft.read().trim().to_owned();
        let client = api.client();
        spawn(async move {
            if client
                .rename_passkey()
                .id(id)
                .body(PasskeyRename { label })
                .send()
                .await
                .is_ok()
            {
                renaming.set(false);
                reload.bump();
            }
            busy.release();
        });
    });

    if *confirming.read() {
        return rsx! {
            InlineConfirm {
                title: i18n.args("passkey.revoke.title", &[("label", &label)]),
                body: i18n.t("passkey.revoke.body"),
                cta: i18n.t("passkey.revoke.cta"),
                busy: busy.is_busy(),
                on_cancel: move |()| confirming.set(false),
                on_confirm: move |()| revoke.call(()),
            }
        };
    }

    rsx! {
        div { class: "ik-row",
            div { class: "grow",
                if *renaming.read() {
                    Field {
                        id: "tv-passkey-rename-{id}",
                        label: i18n.t("passkey.field.label"),
                        value: draft(),
                        on_input: move |v| draft.set(v),
                        on_enter: move |()| save.call(()),
                    }
                } else {
                    div { style: "font-weight:600;font-size:13px;", "{label}" }
                    div { class: "ik-mono ik-muted", style: "font-size:11px;",
                        {i18n.args("passkey.addedOn", &[("date", &created)])}
                        " · "
                        "{used}"
                    }
                }
            }
            div { class: "ik-flex", style: "gap:6px;flex:none;",
                if *renaming.read() {
                    button {
                        class: "ik-btn xs primary",
                        disabled: busy.is_busy(),
                        onclick: move |_| save.call(()),
                        {i18n.t("common.save")}
                    }
                    button {
                        class: "ik-btn xs",
                        onclick: move |_| renaming.set(false),
                        {i18n.t("common.cancel")}
                    }
                } else {
                    button {
                        class: "ik-btn xs",
                        onclick: move |_| renaming.set(true),
                        {i18n.t("passkey.rename")}
                    }
                    button {
                        class: "ik-btn xs",
                        onclick: move |_| confirming.set(true),
                        {i18n.t("passkey.revoke.cta")}
                    }
                }
            }
        }
    }
}
