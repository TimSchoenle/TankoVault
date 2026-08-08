//! The "confirm it is you" prompt a sensitive action puts up before it will run.
//!
//! Rendered inline by the screen that needs it rather than as a global modal, because the
//! screens that need it are all one panel deep and an inline form keeps the action the reader
//! was taking visible behind the question. Whichever factor they present, the grant that comes
//! back goes into [`crate::state::step_up`] and every sensitive call in the rest of the session
//! rides on it until it lapses.
//!
//! It offers the password only to an account with **no** second factor. That is not a
//! convenience: such an account has nothing stronger to present, and refusing it would make the
//! sensitive routes — including the enrolment that would fix that — unreachable. The API refuses
//! the same branch the moment a factor exists, so this is a mirror of the server's rule rather
//! than a second copy of the decision.

use crate::api;
use crate::components::Field;
use crate::hooks::use_busy;
use crate::i18n::use_i18n;
use crate::state::step_up::use_step_up;
use crate::wire::types::StepUpRequest;
use dioxus::prelude::*;
use progenitor_client::ResponseValue;

/// Which factor the reader is being asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StepUpFactor {
    Totp,
    RecoveryCode,
    Password,
}

/// Ask the reader to confirm themselves, and store the grant on success.
///
/// `enrolled` decides what is offered: an account with a second factor is asked for a code (or a
/// recovery code), one without is asked for its password. `on_done` fires once the grant is
/// stored, so the caller can retry whatever it was doing.
#[component]
pub(crate) fn StepUpPrompt(enrolled: bool, on_done: EventHandler<()>) -> Element {
    let i18n = use_i18n();
    let api = api::use_api();
    let step_up = use_step_up();
    let busy = use_busy();

    let mut factor = use_signal(|| {
        if enrolled {
            StepUpFactor::Totp
        } else {
            StepUpFactor::Password
        }
    });
    let mut value = use_signal(String::new);
    let mut error = use_signal(|| Option::<String>::None);

    let submit = use_callback(move |()| {
        if !busy.claim() {
            return;
        }
        error.set(None);
        let entered = value.read().trim().to_owned();
        if entered.is_empty() {
            busy.release();
            return;
        }
        let chosen = *factor.read();
        let client = api.client();

        spawn(async move {
            let body = match chosen {
                StepUpFactor::Totp => StepUpRequest {
                    totp_code: Some(entered),
                    recovery_code: None,
                    password: None,
                },
                StepUpFactor::RecoveryCode => StepUpRequest {
                    totp_code: None,
                    recovery_code: Some(entered),
                    password: None,
                },
                StepUpFactor::Password => StepUpRequest {
                    totp_code: None,
                    recovery_code: None,
                    password: Some(entered),
                },
            };

            match client.step_up().body(body).send().await {
                Ok(res) => {
                    step_up.set(ResponseValue::into_inner(res).token);
                    value.set(String::new());
                    on_done.call(());
                }
                // A `401` here is a wrong factor, not an expired session: the request carried a
                // valid bearer token or it would not have reached the handler. The shared
                // catalogue's "please sign in" would send the reader looking for a session
                // problem that does not exist.
                Err(e) => error.set(Some(match api::error_status(&e) {
                    Some(401) => i18n.t("stepUp.error.wrongFactor"),
                    _ => api::friendly_error(i18n, e),
                })),
            }
            busy.release();
        });
    });

    let (field_label, field_kind, autocomplete, hint) = match *factor.read() {
        StepUpFactor::Totp => (
            i18n.t("stepUp.field.code"),
            "text",
            "one-time-code",
            i18n.t("stepUp.hint.code"),
        ),
        StepUpFactor::RecoveryCode => (
            i18n.t("stepUp.field.recovery"),
            "text",
            "off",
            i18n.t("stepUp.hint.recovery"),
        ),
        StepUpFactor::Password => (
            i18n.t("stepUp.field.password"),
            "password",
            "current-password",
            i18n.t("stepUp.hint.password"),
        ),
    };

    rsx! {
        div { class: "ik-note", style: "padding:12px;margin:12px 0;",
            p { style: "margin:0 0 8px;font-weight:600;", {i18n.t("stepUp.title")} }
            p { class: "ik-muted", style: "font-size:13px;margin:0 0 10px;",
                {i18n.t("stepUp.intro")}
            }

            if let Some(msg) = error.read().clone() {
                div { class: "ik-error", style: "padding:8px;margin-bottom:8px;", "{msg}" }
            }

            Field {
                id: "tv-step-up",
                label: field_label,
                kind: field_kind,
                autocomplete,
                value: value(),
                hint,
                on_input: move |v| value.set(v),
                on_enter: move |()| submit.call(()),
            }

            div { class: "ik-flex", style: "gap:6px;margin-top:8px;",
                button {
                    class: "ik-btn primary",
                    disabled: busy.is_busy(),
                    onclick: move |_| submit.call(()),
                    {i18n.t("stepUp.confirm")}
                }
                // Only an enrolled account has a second option; offering "use a recovery code"
                // to an account with no codes would be a dead end wearing a link's clothes.
                if enrolled {
                    button {
                        class: "ik-btn",
                        onclick: move |_| {
                            let next = if *factor.read() == StepUpFactor::Totp {
                                StepUpFactor::RecoveryCode
                            } else {
                                StepUpFactor::Totp
                            };
                            factor.set(next);
                            value.set(String::new());
                            error.set(None);
                        },
                        if *factor.read() == StepUpFactor::Totp {
                            {i18n.t("stepUp.useRecovery")}
                        } else {
                            {i18n.t("stepUp.useCode")}
                        }
                    }
                }
            }
        }
    }
}
