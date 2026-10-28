//! Owns the sign-in form's state and the four requests it sends: password sign-in or registration,
//! the second factor, a passkey, and a resent confirmation link.

use dioxus::prelude::*;
use dioxus::router::Navigator;
use webauthn_rs_proto::RequestChallengeResponse;

use crate::api::{self, Api};
use crate::hooks::{use_busy, Busy};
use crate::i18n::{use_i18n, Translator};
use crate::models::*;
use crate::state::{use_session, Session};
use crate::views::DiscoverQuery;
use crate::webauthn::{self, CeremonyError};
use crate::wire::types::MfaVerifyRequest;
use crate::Route;

/// The sign-in screen's handles and form fields. Every field is `Copy`, so the screen's
/// callbacks and the requests they spawn each take their own copy.
#[derive(Clone, Copy)]
pub(super) struct SignIn {
    pub(super) session: Session,
    pub(super) i18n: Translator,
    pub(super) nav: Navigator,
    pub(super) busy: Busy,
    pub(super) api: Api,
    pub(super) register_mode: Signal<bool>,
    pub(super) email: Signal<String>,
    pub(super) username: Signal<String>,
    pub(super) login: Signal<String>,
    pub(super) password: Signal<String>,
    pub(super) error: Signal<Option<String>>,
    /// A neutral, non-error status line (e.g. "check your inbox to confirm your email").
    pub(super) info: Signal<Option<String>>,
    /// Set when refused for an unconfirmed address, to surface "resend confirmation".
    pub(super) needs_verification: Signal<bool>,
    /// The handle a half-finished sign-in is resumed by: the password verified and a second
    /// factor is owed. Holding it is safe: it authorises nothing but the attempt to finish, and
    /// **no session was issued** alongside it.
    pub(super) pending_mfa: Signal<Option<String>>,
    pub(super) second_factor: Signal<String>,
    /// Whether the reader is presenting a recovery code instead of an authenticator code.
    pub(super) using_recovery: Signal<bool>,
}

impl SignIn {
    /// The screen's handles and empty form fields.
    pub(super) fn use_new() -> Self {
        Self {
            session: use_session(),
            i18n: use_i18n(),
            nav: use_navigator(),
            busy: use_busy(),
            api: api::use_api(),
            register_mode: use_signal(|| false),
            email: use_signal(String::new),
            username: use_signal(String::new),
            login: use_signal(String::new),
            password: use_signal(String::new),
            error: use_signal(|| None),
            info: use_signal(|| None),
            needs_verification: use_signal(|| false),
            pending_mfa: use_signal(|| None),
            second_factor: use_signal(String::new),
            using_recovery: use_signal(|| false),
        }
    }

    /// Signs in with the typed login and password, or registers with the typed fields.
    pub(super) fn submit(self) {
        let Self {
            i18n,
            session,
            nav,
            busy,
            api,
            mut register_mode,
            email,
            username,
            login,
            mut password,
            mut error,
            mut info,
            mut needs_verification,
            mut pending_mfa,
            ..
        } = self;
        if !busy.claim() {
            return;
        }
        error.set(None);
        info.set(None);
        needs_verification.set(false);
        let is_register = *register_mode.read();
        let email_v = email.read().trim().to_owned();
        let username_v = username.read().trim().to_owned();
        let login_v = login.read().trim().to_owned();
        let password_v = password.read().clone();
        let client = api.client();
        spawn(async move {
            // register and login return different bodies (RegisterResponse vs TokenResponse),
            // so each branch is handled inline instead of through a shared result.
            if is_register {
                match client
                    .register()
                    .body(RegisterRequest {
                        email: email_v,
                        username: username_v,
                        password: password_v,
                    })
                    .send()
                    .await
                {
                    Ok(res) => {
                        let body = res.into_inner();
                        if body.verification_required {
                            // Email delivery is on: the account must be confirmed first.
                            info.set(Some(i18n.t("auth.registered")));
                            register_mode.set(false);
                            password.set(String::new());
                        } else if let Some(token) = body.access_token {
                            // No mailer: the account was activated immediately.
                            session.set_token(token);
                            nav.push(Route::Discover {
                                query: DiscoverQuery::default(),
                            });
                        } else {
                            // Neither outcome: an API that broke its contract must not leave
                            // the form looking as if nothing was sent.
                            error.set(Some(i18n.t("error.unexpected")));
                        }
                    }
                    Err(e) => error.set(Some(api::friendly_error(i18n, e))),
                }
            } else {
                match client
                    .login()
                    .body(LoginRequest {
                        login: login_v,
                        password: password_v,
                    })
                    .send()
                    .await
                {
                    Ok(res) => {
                        let body = res.into_inner();
                        match body.session {
                            // No second factor on this account: signed in, as it always was.
                            Some(session_tokens) => {
                                session.set_token(session_tokens.access_token);
                                nav.push(Route::Discover {
                                    query: DiscoverQuery::default(),
                                });
                            }
                            // A second factor is owed. **No session was issued**: the handle
                            // below authorises nothing except the attempt to finish, so holding
                            // it while the reader finds their phone is safe.
                            None => match body.mfa {
                                Some(challenge) => {
                                    pending_mfa.set(Some(challenge.challenge_token));
                                    password.set(String::new());
                                }
                                // Neither branch present: a server that answered something this
                                // build does not understand. Say so, not silently doing
                                // nothing, which reads as a dead button.
                                None => error.set(Some(i18n.t("error.unexpected"))),
                            },
                        }
                    }
                    Err(e) => match sign_in_failure(api::error_status(&e)) {
                        Some((key, offer_resend)) => {
                            needs_verification.set(offer_resend);
                            error.set(Some(i18n.t(key)));
                        }
                        None => error.set(Some(api::friendly_error(i18n, e))),
                    },
                }
            }
            busy.release();
        });
    }

    /// The second leg: trade the pending sign-in and one factor for the session the first leg
    /// withheld. A wrong code is `401` and simply says so: the challenge survives, up to its
    /// own attempt cap, which the server enforces and this screen does not duplicate.
    pub(super) fn finish_mfa(self) {
        let Self {
            i18n,
            session,
            nav,
            busy,
            api,
            mut error,
            mut pending_mfa,
            mut second_factor,
            using_recovery,
            ..
        } = self;
        if !busy.claim() {
            return;
        }
        error.set(None);
        let Some(handle) = pending_mfa.read().clone() else {
            busy.release();
            return;
        };
        let entered = second_factor.read().trim().to_owned();
        if entered.is_empty() {
            busy.release();
            return;
        }
        let recovery = *using_recovery.read();
        let client = api.client();

        spawn(async move {
            let body = MfaVerifyRequest {
                challenge_token: handle,
                totp_code: (!recovery).then(|| entered.clone()),
                recovery_code: recovery.then_some(entered),
                security_key: None,
            };
            match client.mfa_verify().body(body).send().await {
                Ok(res) => {
                    session.set_token(res.into_inner().access_token);
                    pending_mfa.set(None);
                    second_factor.set(String::new());
                    nav.push(Route::Discover {
                        query: DiscoverQuery::default(),
                    });
                }
                Err(e) => error.set(Some(match api::error_status(&e) {
                    Some(401) => i18n.t("auth.mfa.wrongCode"),
                    _ => api::friendly_error(i18n, e),
                })),
            }
            busy.release();
        });
    }

    /// Signs in with a passkey: the account is resolved from the credential, not anything typed;
    /// see `services/api/src/auth/passkey.rs`.
    pub(super) fn passkey_sign_in(self) {
        let Self {
            i18n,
            session,
            nav,
            busy,
            api,
            mut error,
            mut info,
            mut needs_verification,
            ..
        } = self;
        if !busy.claim() {
            return;
        }
        error.set(None);
        info.set(None);
        needs_verification.set(false);
        let client = api.client();
        spawn(async move {
            let started = match client.passkey_login_start().send().await {
                Ok(res) => res.into_inner(),
                Err(e) => {
                    error.set(Some(api::friendly_error(i18n, e)));
                    busy.release();
                    return;
                }
            };

            let challenge: RequestChallengeResponse =
                match webauthn::parse_challenge(started.options) {
                    Ok(challenge) => challenge,
                    Err(e) => return report(&e, error, busy, i18n),
                };
            let credential = match webauthn::get(challenge).await {
                Ok(credential) => credential,
                Err(e) => return report(&e, error, busy, i18n),
            };
            let envelope = match webauthn::to_envelope(&credential) {
                Ok(envelope) => envelope,
                Err(e) => return report(&e, error, busy, i18n),
            };

            match client
                .passkey_login_finish()
                .body(PasskeyLoginRequest {
                    ceremony_id: started.ceremony_id,
                    credential: envelope,
                })
                .send()
                .await
            {
                Ok(res) => {
                    session.set_token(res.into_inner().access_token);
                    nav.push(Route::Discover {
                        query: DiscoverQuery::default(),
                    });
                }
                // The same two statuses a password sign-in distinguishes, for the same reasons.
                // A `401` here is not "wrong password" though (nothing was typed), so it gets
                // its own sentence naming what actually failed.
                Err(e) => match api::error_status(&e) {
                    Some(401) => error.set(Some(i18n.t("passkey.error.notRecognised"))),
                    Some(403) => {
                        needs_verification.set(true);
                        error.set(Some(i18n.t("auth.confirmFirst")));
                    }
                    _ => error.set(Some(api::friendly_error(i18n, e))),
                },
            }
            busy.release();
        });
    }

    /// Resends the confirmation link to the address entered in the sign-in field. Always
    /// reports success (the endpoint is deliberately silent about whether the account exists).
    pub(super) fn resend(self) {
        let Self {
            i18n,
            api,
            login,
            mut error,
            mut info,
            mut needs_verification,
            busy,
            ..
        } = self;
        let email_v = login.read().trim().to_owned();
        if email_v.is_empty() || !busy.claim() {
            return;
        }
        let client = api.client();
        spawn(async move {
            let _ = client
                .resend_verification()
                .body(ResendVerificationRequest { email: email_v })
                .send()
                .await;
            needs_verification.set(false);
            error.set(None);
            info.set(Some(i18n.t("auth.resent")));
            busy.release();
        });
    }
}

/// How a failed sign-in should be worded, and whether to offer "resend confirmation".
///
/// Returns the catalogue key and the resend flag, or `None` when [`api::friendly_error`]'s
/// generic wording is right. Split out from the submit callback so the mapping is testable
/// without a Dioxus runtime.
///
/// `401` here is "bad credentials", not the shared catalogue's generic 401 wording, which would
/// tell the reader to do the thing they're already doing. `403` means the password was right but
/// the address isn't confirmed, so it's the only status that offers the resend action.
fn sign_in_failure(status: Option<u16>) -> Option<(&'static str, bool)> {
    match status? {
        401 => Some(("auth.badCredentials", false)),
        403 => Some(("auth.confirmFirst", true)),
        _ => None,
    }
}

/// Words a failed passkey ceremony, or says nothing when the reader simply cancelled.
///
/// Not shared with `views::account::passkeys`: same rule, different signals, and threading
/// them through one function costs more indirection than it saves.
/// [`CeremonyError::Cancelled`] clears the error line instead of writing to it: a ceremony the
/// reader chose to stop isn't a broken feature.
fn report(
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A rejected sign-in must not be worded "You need to sign in to do that."
    ///
    /// The bug: that is the shared catalogue's sentence for *any* 401, and the sign-in form
    /// showed it when the password was wrong: the one screen where it is meaningless, because
    /// the reader is already signing in and the sentence names nothing to correct. It reads like
    /// a session or routing fault, so it sends a search towards the auth middleware and the
    /// proxy, when the API has answered correctly and audited the attempt as `bad_password`.
    ///
    /// The generic wording stays right everywhere else, so the wording is per-screen and this
    /// test pins the screen, not the catalogue.
    #[test]
    fn a_rejected_sign_in_is_worded_as_bad_credentials() {
        assert_eq!(
            sign_in_failure(Some(401)),
            Some(("auth.badCredentials", false))
        );
    }

    /// 403 is the neighbouring case and must stay distinguishable from 401: the password was
    /// accepted and only the address is unconfirmed, so this is the one status that offers
    /// the resend action. Collapsing the two would make a confirmable account look like a
    /// typo'd password.
    #[test]
    fn an_unconfirmed_address_still_offers_the_resend_action() {
        assert_eq!(
            sign_in_failure(Some(403)),
            Some(("auth.confirmFirst", true))
        );
    }

    /// Anything else falls through to `api::friendly_error`, which buckets transport faults
    /// and undocumented statuses into plain language. `None` for a missing status matters:
    /// that is a transport fault, where there is no status to word at all.
    #[test]
    fn other_failures_fall_through_to_the_generic_wording() {
        assert_eq!(sign_in_failure(Some(429)), None);
        assert_eq!(sign_in_failure(Some(500)), None);
        assert_eq!(sign_in_failure(None), None);
    }
}
