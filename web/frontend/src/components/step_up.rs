//! The "confirm it is you" prompt a sensitive action puts up before it will run.
//!
//! **A modal, and deliberately the one thing on screen.** It used to render inline in the panel
//! that needed it, on the theory that keeping the reader's work visible behind the question was
//! kinder. What it actually produced was a button that appeared to do nothing: the demand
//! arrived as a note somewhere in a long panel, sometimes below the fold, and the action the
//! reader had asked for simply never happened. A question that gates work has to be answered,
//! so it takes the screen until it is.
//!
//! **Answering it resumes the action.** The gate remembers what was attempted
//! ([`StepUpGate::attempt`]) and replays it once the grant lands, because "confirmed — now click
//! it again" is a second thing to do for a question the reader did not ask to be asked.
//!
//! Whichever factor they present, the grant that comes back goes into [`crate::state::step_up`]
//! and every sensitive call in the rest of the session rides on it until it lapses. The window
//! is an idle one server-side, so working on through a panel keeps it alive.
//!
//! **What it offers is read off the account, not assumed.** The factors an account actually holds
//! are whatever it enrolled — a security key, an authenticator app, or both — so the prompt asks
//! `GET /v1/me/mfa` and offers those. Guessing produced a dead end: an account with only a
//! security key was shown a box for an authenticator code it had no way to produce.
//!
//! It offers the password only to an account with **no** second factor. That is not a
//! convenience: such an account has nothing stronger to present, and refusing it would make the
//! sensitive routes — including the enrolment that would fix that — unreachable. The API refuses
//! the same branch the moment a factor exists, so this is a mirror of the server's rule rather
//! than a second copy of the decision.

use crate::api::{self, Refusal};
use crate::components::Field;
use crate::hooks::use_busy;
use crate::i18n::use_i18n;
use crate::icons::{Ic, Icon};
use crate::state::step_up::{use_step_up, StepUp};
use crate::webauthn::{self, CeremonyError};
use crate::wire::types::{MfaStatus, SecurityKeyAssertion, StepUpRequest};
use dioxus::prelude::*;
use progenitor_client::ResponseValue;
use std::cell::RefCell;
use std::rc::Rc;
use webauthn_rs_proto::RequestChallengeResponse;

/// Which factor the reader is being asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StepUpFactor {
    SecurityKey,
    Totp,
    RecoveryCode,
    Password,
}

impl StepUpFactor {
    /// The catalogue key for the link that switches *to* this factor.
    const fn switch_key(self) -> &'static str {
        match self {
            Self::SecurityKey => "stepUp.useKey",
            Self::Totp => "stepUp.useCode",
            Self::RecoveryCode => "stepUp.useRecovery",
            Self::Password => "stepUp.usePassword",
        }
    }
}

/// The factors this account can present, most preferred first.
///
/// `fallback_enrolled` is only consulted while the status is unknown: the deployment can switch
/// the `/v1/me/mfa` routes off without disarming the factors already enrolled, in which case the
/// probe 404s but step-up itself still works.
///
/// A security key is offered ahead of a typed code because it is both the stronger proof and the
/// shorter path — one touch against six digits copied from a phone.
fn offered_factors(
    fallback_enrolled: bool,
    status: Option<&MfaStatus>,
    key_available: bool,
) -> Vec<StepUpFactor> {
    let Some(status) = status else {
        return if fallback_enrolled {
            vec![StepUpFactor::Totp, StepUpFactor::RecoveryCode]
        } else {
            vec![StepUpFactor::Password]
        };
    };
    if !status.enrolled {
        return vec![StepUpFactor::Password];
    }

    let mut offered = Vec::new();
    // Registered *and* usable here: a key registered from a browser is no help to a desktop
    // build with no authenticator API, and offering it would be a button that cannot work.
    if key_available && !status.security_keys.is_empty() {
        offered.push(StepUpFactor::SecurityKey);
    }
    if status.totp_confirmed_at.is_some() {
        offered.push(StepUpFactor::Totp);
    }
    // Always last, and always present: an enrolled account has recovery codes, and they are the
    // way through when the factor above is the thing that was lost.
    offered.push(StepUpFactor::RecoveryCode);
    offered
}

/// The prompt, mounted for the whole screen and rendering only once the gate has been refused.
///
/// One line per screen: `StepUpGuard { gate }`. Confirming it replays whatever
/// [`StepUpGate::attempt`] last recorded, so the caller has nothing to do on success — screens
/// that also need to say something can still pass `on_done`, which runs after the replay.
///
/// `intro` replaces the default sentence, which says the action changes *your account* — true of
/// every reader-facing use and of none of the operator ones. `enrolled` is the caller's best
/// guess at whether the account holds a factor, used only until the account's real factor list
/// arrives; every operator surface is behind a permission that implies enrolment.
#[component]
pub(crate) fn StepUpGuard(
    gate: StepUpGate,
    #[props(default)] intro: Option<String>,
    #[props(default = true)] enrolled: bool,
    #[props(default)] on_done: Option<EventHandler<()>>,
) -> Element {
    rsx! {
        if gate.is_open() {
            StepUpDialog {
                enrolled,
                intro: intro.clone(),
                on_done: move |()| {
                    gate.confirmed();
                    if let Some(handler) = on_done {
                        handler.call(());
                    }
                },
                on_cancel: move |()| gate.cancel(),
            }
        }
    }
}

/// The dialog itself: mounted only while the question is open, so the factor probe below runs
/// when it is asked rather than on every screen that *might* ask.
#[component]
fn StepUpDialog(
    enrolled: bool,
    intro: Option<String>,
    on_done: EventHandler<()>,
    on_cancel: EventHandler<()>,
) -> Element {
    let i18n = use_i18n();
    let api = api::use_api();
    let step_up = use_step_up();
    let busy = use_busy();

    // Unelevated on purpose: this is the read that decides what to ask for, and demanding an
    // elevation to learn how to earn one would be a loop.
    let status = use_resource(move || {
        let client = api.client();
        async move {
            client
                .mfa_status()
                .send()
                .await
                .map(ResponseValue::into_inner)
                .ok()
        }
    });

    // `None` means "whatever is offered first", so the default follows the factor list in when
    // it arrives. An explicit pick writes `Some` and is never overwritten by it.
    let mut chosen = use_signal(|| Option::<StepUpFactor>::None);
    let mut value = use_signal(String::new);
    let mut error = use_signal(|| Option::<String>::None);

    let submit = use_callback(move |factor: StepUpFactor| {
        if !busy.claim() {
            return;
        }
        error.set(None);
        let entered = value.read().trim().to_owned();
        if entered.is_empty() {
            busy.release();
            return;
        }
        let client = api.client();

        spawn(async move {
            let body = match factor {
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
                // The password is the only typed factor left. `SecurityKey` rides along
                // unreachably — it has no field to submit and `assert_key` below instead — and
                // arrives here only if a future factor forgets to pick a branch.
                StepUpFactor::SecurityKey | StepUpFactor::Password => StepUpRequest {
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

    let assert_key = use_callback(move |()| {
        if !busy.claim() {
            return;
        }
        error.set(None);
        let client = api.client();

        spawn(async move {
            let started = match client.step_up_security_key_start().send().await {
                Ok(res) => ResponseValue::into_inner(res),
                Err(e) => {
                    error.set(Some(api::friendly_error(i18n, e)));
                    busy.release();
                    return;
                }
            };

            // The authenticator. Chained into one fallible block so the outcome reaches the
            // screen once: a reader who walked away from the prompt has changed nothing, and
            // must not be shown three different ways of saying so.
            let ceremony = async {
                let challenge: RequestChallengeResponse =
                    webauthn::parse_challenge(started.options)?;
                let credential = webauthn::get(challenge).await?;
                webauthn::to_envelope(&credential)
            }
            .await;
            let credential = match ceremony {
                Ok(credential) => credential,
                Err(outcome) => {
                    // A ceremony the reader chose to stop is not a failure to word as one.
                    if !matches!(outcome, CeremonyError::Cancelled) {
                        error.set(Some(i18n.t(outcome.key())));
                    }
                    busy.release();
                    return;
                }
            };

            match client
                .step_up_security_key_finish()
                .body(SecurityKeyAssertion {
                    ceremony_id: started.ceremony_id,
                    credential,
                })
                .send()
                .await
            {
                Ok(res) => {
                    step_up.set(ResponseValue::into_inner(res).token);
                    on_done.call(());
                }
                // See the `submit` callback: a `401` is the assertion being refused, not the
                // session.
                Err(e) => error.set(Some(match api::error_status(&e) {
                    Some(401) => i18n.t("stepUp.error.keyRejected"),
                    _ => api::friendly_error(i18n, e),
                })),
            }
            busy.release();
        });
    });

    let settled = status.read().is_some();
    let offered = offered_factors(
        enrolled,
        status.read().as_ref().and_then(Option::as_ref),
        webauthn::is_available(),
    );
    // An explicit pick wins, but only while the account still offers it. `offered` is never
    // empty; the final fallback keeps this total without an unwrap.
    let current = (*chosen.read())
        .filter(|f| offered.contains(f))
        .or_else(|| offered.first().copied())
        .unwrap_or(StepUpFactor::RecoveryCode);

    rsx! {
        div { class: "ik-stepup-scrim",
            div {
                class: "ik-stepup",
                role: "dialog",
                "aria-modal": "true",
                "aria-labelledby": "tv-step-up-title",
                // Focusable so `Escape` reaches the dialog before anything is typed; the field
                // below takes focus off it the moment the factor list settles.
                tabindex: "-1",
                onmounted: move |event| {
                    let element = event.data();
                    spawn(async move {
                        let _ = element.set_focus(true).await;
                    });
                },
                // No click-outside dismissal, unlike the settings sheet: this one is answered
                // mid-typing, and a stray click on the scrim would discard a half-entered code
                // *and* the action waiting behind it. `Escape` and the button are the ways out.
                onkeydown: move |event| {
                    if event.key() == Key::Escape {
                        on_cancel.call(());
                    }
                },

                div { class: "ik-stepup-head",
                    span { class: "ik-stepup-mark", Ic { icon: Icon::ShieldLock, size: 18 } }
                    h2 { id: "tv-step-up-title", {i18n.t("stepUp.title")} }
                }
                p { class: "ik-stepup-intro",
                    {intro.clone().unwrap_or_else(|| i18n.t("stepUp.intro"))}
                }

                if let Some(msg) = error.read().clone() {
                    div { class: "ik-error", style: "padding:9px 11px;margin-bottom:11px;text-align:left;",
                        "{msg}"
                    }
                }

                // Nothing is asked until the factor list has settled. A field rendered on the guess
                // and swapped a moment later would ask half the accounts here for the wrong thing.
                if settled {
                    if current == StepUpFactor::SecurityKey {
                        p { class: "ik-muted", style: "font-size:13px;margin:0;",
                            {i18n.t("stepUp.hint.key")}
                        }
                    } else {
                        {
                            let (label, kind, autocomplete, hint) = match current {
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
                                StepUpFactor::SecurityKey | StepUpFactor::Password => (
                                    i18n.t("stepUp.field.password"),
                                    "password",
                                    "current-password",
                                    i18n.t("stepUp.hint.password"),
                                ),
                            };
                            rsx! {
                                Field {
                                    id: "tv-step-up",
                                    label,
                                    kind,
                                    autocomplete,
                                    value: value(),
                                    hint,
                                    autofocus: true,
                                    on_input: move |v| value.set(v),
                                    on_enter: move |()| submit.call(current),
                                }
                            }
                        }
                    }

                    div { class: "ik-stepup-actions",
                        button {
                            class: "ik-btn",
                            r#type: "button",
                            onclick: move |_| on_cancel.call(()),
                            {i18n.t("common.cancel")}
                        }
                        button {
                            class: "ik-btn primary",
                            r#type: "button",
                            disabled: busy.is_busy(),
                            onclick: move |_| {
                                if current == StepUpFactor::SecurityKey {
                                    assert_key.call(());
                                } else {
                                    submit.call(current);
                                }
                            },
                            if current == StepUpFactor::SecurityKey {
                                {i18n.t("stepUp.confirmWithKey")}
                            } else {
                                {i18n.t("stepUp.confirm")}
                            }
                        }
                    }

                    // One link per *other* factor the account holds. Offering a switch to
                    // something it does not hold would be a dead end wearing a link's clothes.
                    if offered.len() > 1 {
                        div { class: "ik-stepup-alts",
                            for factor in offered.iter().copied().filter(|f| *f != current) {
                                button {
                                    key: "{factor:?}",
                                    class: "ik-btn bare",
                                    r#type: "button",
                                    onclick: move |_| {
                                        chosen.set(Some(factor));
                                        value.set(String::new());
                                        error.set(None);
                                    },
                                    {i18n.t(factor.switch_key())}
                                }
                            }
                        }
                    }

                    p { class: "ik-stepup-hold",
                        Ic { icon: Icon::Check, size: 13 }
                        span { {i18n.t("stepUp.holds")} }
                    }
                }
            }
        }
    }
}

/// What a screen was doing when the API demanded an elevation, held until it can be redone.
///
/// `Rc` rather than a `Callback`: the actions this replays take arguments — a provider id, a row
/// — and are invoked from half a dozen shapes of handler, so the invocation site closes over its
/// own arguments and hands the gate something it can simply call.
///
/// `FnMut` behind a `RefCell` because writing a signal takes `&mut`, and every one of these
/// actions writes at least one: a busy latch, an outcome line, a form it clears.
type PendingAction = Rc<RefCell<dyn FnMut()>>;

/// The "a `403` means confirm, not refuse" rule, held once per screen that needs it.
///
/// Every sensitive call is the same four steps — send with whatever elevation is held, read a
/// `403` as a prompt rather than a refusal, confirm, run the thing that was asked for — and each
/// screen that spelled them out itself got one of them wrong. The privacy panel reported "you
/// don't have permission to do that" for a download the reader was entitled to; the session list
/// swallowed the refusal and did nothing at all; and every screen that got the prompt right
/// still made the reader click the button a second time afterwards.
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct StepUpGate {
    step_up: StepUp,
    prompting: Signal<bool>,
    pending: Signal<Option<PendingAction>>,
}

impl StepUpGate {
    /// A client carrying the current elevation, for the call the gate protects.
    pub(crate) fn client(self, api: api::Api) -> tankovault_api_client::Client {
        self.step_up.client(api)
    }

    /// Run a guarded action, remembering it in case the API asks who is doing it.
    ///
    /// Every invocation of a guarded action goes through here, including the ones that will
    /// succeed: what the gate replays has to be the thing that was actually attempted, and the
    /// only place that knows it — with its arguments bound — is the handler that started it.
    pub(crate) fn attempt(mut self, action: impl FnMut() + 'static) {
        let action: PendingAction = Rc::new(RefCell::new(action));
        self.pending.set(Some(Rc::clone(&action)));
        (*action.borrow_mut())();
    }

    /// Route a failed call. `true` means it was a step-up demand and the prompt is now open, so
    /// the caller reports nothing; `false` means the failure is the caller's to word.
    ///
    /// Takes the *classified* refusal rather than the error: every generated operation has its
    /// own error type, so one method could not accept them all — and one is the point.
    ///
    /// It is [`Refusal`] rather than the bare status because the guarded surfaces answer `403`
    /// three ways, and only one of them is a question this prompt can answer. Opening for the
    /// other two would leave an operator confirming themselves against a refusal that a
    /// confirmation does not change.
    #[must_use]
    pub(crate) fn refused(mut self, refusal: Refusal) -> bool {
        if refusal != Refusal::StepUp {
            return false;
        }
        // A grant the API has stopped honouring is worse than none: the screen would keep
        // retrying with it instead of prompting.
        self.step_up.clear();
        self.prompting.set(true);
        true
    }

    /// Whether the prompt should be on screen.
    pub(crate) fn is_open(self) -> bool {
        *self.prompting.read()
    }

    /// The grant is in hand: close the prompt and finish what was interrupted.
    ///
    /// The pending action is taken out before it runs, so a second refusal from the replay
    /// re-records rather than stacking, and a `cancel` cannot fire it a second time.
    pub(crate) fn confirmed(mut self) {
        self.prompting.set(false);
        let action = self.pending.write().take();
        if let Some(action) = action {
            (*action.borrow_mut())();
        }
    }

    /// The reader declined. Nothing ran, and the interrupted action is forgotten rather than
    /// left to fire at whatever the *next* confirmation was for.
    pub(crate) fn cancel(mut self) {
        self.prompting.set(false);
        self.pending.set(None);
    }
}

/// A [`StepUpGate`] scoped to the calling component.
pub(crate) fn use_step_up_gate() -> StepUpGate {
    StepUpGate {
        step_up: use_step_up(),
        // Opened by the server's `403`, never pre-emptively — a reader who came to look should
        // not be challenged before they have asked for anything.
        prompting: use_signal(|| false),
        pending: use_signal(|| None),
    }
}

#[cfg(test)]
mod tests {
    use super::{offered_factors, StepUpFactor};
    use crate::wire::types::{MfaStatus, SecurityKeyDto};

    fn status(totp: bool, keys: usize) -> MfaStatus {
        MfaStatus {
            enrolled: totp || keys > 0,
            recovery_codes_remaining: 8,
            required: false,
            security_keys: (0..keys)
                .map(|i| SecurityKeyDto {
                    id: uuid::Uuid::from_u128(i as u128),
                    label: "key".to_owned(),
                    created_at: "2026-01-01T00:00:00Z".to_owned(),
                    last_used_at: None,
                })
                .collect(),
            totp_available: true,
            totp_confirmed_at: totp.then(|| "2026-01-01T00:00:00Z".to_owned()),
        }
    }

    /// The bug: the prompt offered the authenticator code and nothing else, so an account whose
    /// only factor is a security key was asked for a code it had no way to produce — and every
    /// sensitive action was unreachable behind a box it could not fill in.
    #[test]
    fn a_key_only_account_is_asked_for_its_key() {
        let offered = offered_factors(true, Some(&status(false, 1)), true);
        assert_eq!(offered.first(), Some(&StepUpFactor::SecurityKey));
        assert!(
            !offered.contains(&StepUpFactor::Totp),
            "an authenticator code it cannot produce is a dead end"
        );
    }

    /// Both factors enrolled: both offered, and the key first — it is the stronger proof and the
    /// shorter path.
    #[test]
    fn both_factors_are_offered_key_first() {
        assert_eq!(
            offered_factors(true, Some(&status(true, 1)), true),
            vec![
                StepUpFactor::SecurityKey,
                StepUpFactor::Totp,
                StepUpFactor::RecoveryCode
            ]
        );
    }

    /// A key registered elsewhere is no help on a build with no authenticator API. Offering it
    /// would be a button whose only outcome is `CeremonyError::Unsupported`.
    #[test]
    fn a_key_is_not_offered_where_no_ceremony_can_run() {
        let offered = offered_factors(true, Some(&status(true, 2)), false);
        assert!(!offered.contains(&StepUpFactor::SecurityKey));
        assert_eq!(offered.first(), Some(&StepUpFactor::Totp));
    }

    /// The password is what an account with no factor has, and the *only* thing the server will
    /// take from it. Offering a code it has not enrolled would refuse it its own account.
    #[test]
    fn an_unenrolled_account_is_asked_for_its_password() {
        assert_eq!(
            offered_factors(true, Some(&status(false, 0)), true),
            vec![StepUpFactor::Password],
            "the account's own answer beats the caller's guess"
        );
        assert_eq!(
            offered_factors(false, None, true),
            vec![StepUpFactor::Password]
        );
    }

    /// Every factor's switch link has to resolve, or the button renders its own catalogue key.
    #[test]
    fn every_factor_names_a_shipped_string() {
        for factor in [
            StepUpFactor::SecurityKey,
            StepUpFactor::Totp,
            StepUpFactor::RecoveryCode,
            StepUpFactor::Password,
        ] {
            assert!(
                crate::i18n::has_key(factor.switch_key()),
                "{factor:?} switches to an unshipped key"
            );
        }
        for key in [
            "stepUp.confirmWithKey",
            "stepUp.hint.key",
            "stepUp.error.keyRejected",
            "stepUp.holds",
            "common.cancel",
        ] {
            assert!(crate::i18n::has_key(key), "{key} is not in the catalogue");
        }
    }

    /// The MFA routes can be switched off with factors still enrolled, so the probe 404s while
    /// step-up itself keeps working. What every enrolled account holds is still offered.
    #[test]
    fn an_unknown_status_falls_back_to_what_every_account_holds() {
        assert_eq!(
            offered_factors(true, None, true),
            vec![StepUpFactor::Totp, StepUpFactor::RecoveryCode]
        );
    }
}
