//! The two-factor card's state and the requests it sends: enrol and confirm an authenticator
//! app, register a security key, regenerate recovery codes, and remove the authenticator.

use dioxus::prelude::*;
use progenitor_client::ResponseValue;
use webauthn_rs_proto::CreationChallengeResponse;

use crate::api::{self, Api};
use crate::components::{use_step_up_gate, StepUpGate};
use crate::hooks::{use_busy, use_reload, Busy, Reload};
use crate::i18n::{use_i18n, Translator};
use crate::state::step_up::{use_step_up, StepUp};
use crate::webauthn::{self, CeremonyError};
use crate::wire::types::{SecurityKeyRegisterFinish, SecurityKeyRegisterStart, TotpConfirm};

/// The card's handles and the state its requests write. Every field is `Copy`, so each callback
/// and each spawned request takes its own copy.
#[derive(Clone, Copy)]
pub(super) struct Mfa {
    pub(super) i18n: Translator,
    pub(super) api: Api,
    pub(super) reload: Reload,
    pub(super) busy: Busy,
    /// Both: the gate drives the prompt, and removing a factor has to drop the grant that factor
    /// earned — the server revokes it, so keeping it here would only mean retrying with a dead
    /// token.
    pub(super) step_up: StepUp,
    pub(super) gate: StepUpGate,
    pub(super) error: Signal<Option<String>>,
    pub(super) info: Signal<Option<String>>,
    /// The one and only sight of a freshly issued secret.
    pub(super) pending_secret: Signal<Option<(String, String)>>,
    pub(super) confirm_code: Signal<String>,
    /// The one and only sight of a recovery-code set.
    pub(super) fresh_codes: Signal<Vec<String>>,
    pub(super) key_label: Signal<String>,
}

impl Mfa {
    /// The card's handles and empty state.
    pub(super) fn use_new() -> Self {
        Self {
            i18n: use_i18n(),
            api: api::use_api(),
            reload: use_reload(),
            busy: use_busy(),
            step_up: use_step_up(),
            gate: use_step_up_gate(),
            error: use_signal(|| None),
            info: use_signal(|| None),
            pending_secret: use_signal(|| None),
            confirm_code: use_signal(String::new),
            fresh_codes: use_signal(Vec::new),
            key_label: use_signal(String::new),
        }
    }

    /// Report a refused write: open the elevation prompt for a 403, or show the message.
    ///
    /// Every write funnels through this, so the "a 403 means prompt, not fail" rule is written
    /// once. A handler that reported the raw problem instead would show the reader "insufficient
    /// privileges" for an action they are perfectly entitled to take.
    ///
    /// Takes the *classified* refusal and the message rather than the error itself: every
    /// generated operation has its own error type, so one function cannot accept them all — and
    /// one function is the point.
    pub(super) fn handle_refusal(self, (refused, message): (api::Refusal, String)) {
        let Self {
            gate, mut error, ..
        } = self;
        if gate.refused(refused) {
            error.set(None);
        } else {
            error.set(Some(message));
        }
    }

    /// Start enrolling an authenticator app: the one response that carries its secret.
    pub(super) fn start_app_enrolment(self) {
        let Self {
            i18n,
            api,
            busy,
            gate,
            mut error,
            mut info,
            mut pending_secret,
            ..
        } = self;
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
                Err(e) => self.handle_refusal(refusal(i18n, e)),
            }
            busy.release();
        });
    }

    /// Confirm the authenticator app with its first code, which issues the recovery codes.
    pub(super) fn finish_app_enrolment(self) {
        let Self {
            i18n,
            api,
            reload,
            busy,
            mut error,
            mut info,
            mut pending_secret,
            mut confirm_code,
            mut fresh_codes,
            ..
        } = self;
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
    }

    /// Register a security key: start the ceremony, run it on the authenticator, and finish it.
    pub(super) fn register_key(self) {
        let Self {
            i18n,
            api,
            reload,
            busy,
            gate,
            mut error,
            mut info,
            mut fresh_codes,
            mut key_label,
            ..
        } = self;
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
                    self.handle_refusal(refusal(i18n, e));
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
    }

    /// Replace the recovery codes with a fresh set.
    pub(super) fn regenerate(self) {
        let Self {
            i18n,
            api,
            reload,
            busy,
            gate,
            mut error,
            mut fresh_codes,
            ..
        } = self;
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
                Err(e) => self.handle_refusal(refusal(i18n, e)),
            }
            busy.release();
        });
    }

    /// Remove the authenticator app.
    pub(super) fn remove_totp(self) {
        let Self {
            i18n,
            api,
            reload,
            busy,
            step_up,
            gate,
            mut error,
            mut info,
            ..
        } = self;
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
                Err(e) => self.handle_refusal(refusal(i18n, e)),
            }
            busy.release();
        });
    }
}

pub(super) fn refusal(
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
