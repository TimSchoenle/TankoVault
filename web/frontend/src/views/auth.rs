//! Login / Register (§17.4 auth flow). Access token is held in memory; the refresh token
//! is set as an httpOnly cookie by the API. On success we route to Discover.

use crate::api;
use crate::components::Field;
use crate::hooks::use_busy;
use crate::i18n::use_i18n;
use crate::icons::{Ic, Icon};
use crate::models::*;
use crate::state::use_session;
use crate::Route;
use dioxus::prelude::*;

/// How a failed sign-in should be worded, and whether to offer "resend confirmation".
///
/// Returns the catalogue key plus the resend flag, or `None` when the status has nothing
/// sign-in-specific to say and [`api::friendly_error`]'s generic wording is right.
///
/// Split out from the submit callback for the reason `api::error::status_key` is split out
/// from `status_message`: the mapping is the part worth testing, and testing it must not
/// require a Dioxus runtime.
///
/// `401` is worded here rather than left to [`api::friendly_error`] because the shared
/// catalogue entry for 401 is "You need to sign in to do that." That is right on every other
/// screen and nonsense on this one: it tells the reader to do the thing they are already
/// doing, and names nothing they could correct. On the sign-in form a 401 has exactly one
/// meaning — the identifier and password did not match an account.
///
/// `403` is the neighbouring case: the password *was* right, the address just isn't confirmed
/// yet, which is why it is the only status that offers the resend action.
fn sign_in_failure(status: Option<u16>) -> Option<(&'static str, bool)> {
    match status? {
        401 => Some(("auth.badCredentials", false)),
        403 => Some(("auth.confirmFirst", true)),
        _ => None,
    }
}

#[component]
pub(crate) fn Login() -> Element {
    let session = use_session();
    let i18n = use_i18n();
    let nav = use_navigator();

    let mut register_mode = use_signal(|| false);
    let mut email = use_signal(String::new);
    let mut username = use_signal(String::new);
    let mut login = use_signal(String::new);
    let mut password = use_signal(String::new);
    let mut error = use_signal(|| Option::<String>::None);
    // A neutral, non-error status line (e.g. "check your inbox to confirm your email").
    let mut info = use_signal(|| Option::<String>::None);
    // Set when a sign-in was refused because the address isn't confirmed yet, so we can
    // surface a "resend confirmation email" action.
    let mut needs_verification = use_signal(|| false);
    let busy = use_busy();
    // `Api` is `Copy`, so both callbacks below capture the same handle without cloning, and
    // each resolves the live bearer token when it actually fires.
    let api = api::use_api();

    let submit = use_callback(move |()| {
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
            // so each branch is handled inline rather than through a shared result.
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
                            nav.push(Route::Discover {});
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
                        session.set_token(res.into_inner().access_token);
                        nav.push(Route::Discover {});
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
    });

    // Resend the confirmation link to the address entered in the sign-in field. Always
    // reports success (the endpoint is deliberately silent about whether the account exists).
    let resend = use_callback(move |()| {
        let email_v = login.read().trim().to_owned();
        if email_v.is_empty() {
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
        });
    });

    let is_register = *register_mode.read();
    let heading = i18n.t(if is_register {
        "auth.register.heading"
    } else {
        "auth.signIn.heading"
    });
    let cta = i18n.t(if is_register {
        "auth.register.cta"
    } else {
        "common.signIn"
    });
    let toggle_label = i18n.t(if is_register {
        "auth.register.toggle"
    } else {
        "auth.signIn.toggle"
    });
    let subtitle = i18n.t(if is_register {
        "auth.register.subtitle"
    } else {
        "auth.signIn.subtitle"
    });

    rsx! {
        div { class: "ik-auth",
            AuthBrand {}
            h1 { "{heading}" }
            p { class: "ik-muted", "{subtitle}" }

            if let Some(msg) = error.read().clone() {
                div { class: "ik-error", style: "padding:12px;margin:14px 0;text-align:left;",
                    "{msg}"
                }
            }

            if let Some(msg) = info.read().clone() {
                div { class: "ik-note", style: "padding:12px;margin:14px 0;text-align:left;",
                    "{msg}"
                }
            }

            if *needs_verification.read() {
                button {
                    class: "ik-btn",
                    style: "width:100%;margin:0 0 14px;",
                    r#type: "button",
                    onclick: move |_| resend.call(()),
                    {i18n.t("auth.resendConfirmation")}
                }
            }

            if is_register {
                Field {
                    id: "tv-auth-email",
                    label: i18n.t("auth.field.email"),
                    kind: "email",
                    autocomplete: "email",
                    value: email(),
                    on_input: move |v| email.set(v),
                    on_enter: move |()| submit.call(()),
                }
                Field {
                    id: "tv-auth-username",
                    label: i18n.t("auth.field.username"),
                    autocomplete: "username",
                    value: username(),
                    on_input: move |v| username.set(v),
                    on_enter: move |()| submit.call(()),
                }
            } else {
                Field {
                    id: "tv-auth-login",
                    label: i18n.t("auth.field.emailOrUsername"),
                    autocomplete: "username",
                    value: login(),
                    on_input: move |v| login.set(v),
                    on_enter: move |()| submit.call(()),
                }
            }
            Field {
                id: "tv-auth-password",
                label: i18n.t("auth.field.password"),
                kind: "password",
                // `new-password` on the register form is what tells a password manager to
                // offer to *generate* one rather than fill the existing one.
                autocomplete: if is_register { "new-password" } else { "current-password" },
                value: password(),
                on_input: move |v| password.set(v),
                on_enter: move |()| submit.call(()),
            }

            if !is_register {
                div { style: "text-align:right;margin:-4px 0 14px;",
                    Link {
                        to: Route::ForgotPassword {},
                        class: "ik-link",
                        {i18n.t("auth.forgotPassword")}
                    }
                }
            }

            button {
                class: "ik-btn primary",
                style: "width:100%;",
                disabled: busy.is_busy(),
                onclick: move |_| submit.call(()),
                if busy.is_busy() {
                    {i18n.t("common.working")}
                } else {
                    "{cta}"
                }
            }

            button {
                class: "ik-btn",
                style: "width:100%;margin-top:10px;",
                r#type: "button",
                onclick: move |_| {
                    error.set(None);
                    let now = *register_mode.read();
                    register_mode.set(!now);
                },
                "{toggle_label}"
            }
        }
    }
}

/// The shared brand lockup (§7.9) shown atop every auth screen: gradient tile + wordmark +
/// tagline. Extracted so the confirmation and password screens match the sign-in card.
#[component]
pub(crate) fn AuthBrand() -> Element {
    let i18n = use_i18n();
    rsx! {
        div { class: "ik-auth-brand",
            div { class: "ik-brand-tile", Ic { icon: Icon::MenuBook, size: 22 } }
            div {
                // The wordmark is the product's name, not a message — see `nav::Rail`.
                div { class: "ik-wordmark",
                    "Tankō"
                    span { class: "acc", "Vault" }
                }
                div { class: "ik-brand-tag", {i18n.t("nav.tagline")} }
            }
        }
    }
}

/// Email-confirmation landing page for the link in the sign-up email
/// (`/verify-email?token=…`). Confirms the token on mount and, on success, adopts the issued
/// session and drops the user straight into the app.
#[component]
pub(crate) fn VerifyEmail(token: String) -> Element {
    let session = use_session();
    let i18n = use_i18n();
    let nav = use_navigator();
    let api = api::use_api();

    // Fire the confirmation once for this token; `use_resource` re-runs only if `token`
    // changes, so a stale link isn't retried on every render.
    let token_for_call = token.clone();
    let resource = use_resource(move || {
        let client = api.client();
        let token = token_for_call.clone();
        async move {
            client
                .verify_email()
                .body(VerifyEmailRequest { token })
                .send()
                .await
                .map(|r| r.into_inner().access_token)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });

    // On success the API issued a session (and set the refresh cookie); adopt the access
    // token and route into the library, mirroring a fresh sign-in.
    use_effect(move || {
        if let Some(Ok(access_token)) = resource.read().clone() {
            session.set_token(access_token);
            nav.push(Route::Discover {});
        }
    });

    let state = resource.read().clone();
    rsx! {
        div { class: "ik-auth",
            AuthBrand {}
            h1 { {i18n.t("verifyEmail.heading")} }
            match state {
                None => rsx! {
                    p { class: "ik-muted", {i18n.t("verifyEmail.pending")} }
                },
                Some(Ok(_)) => rsx! {
                    p { class: "ik-muted", {i18n.t("verifyEmail.confirmed")} }
                },
                Some(Err(msg)) => rsx! {
                    div { class: "ik-error", style: "padding:12px;margin:14px 0;text-align:left;",
                        "{msg}"
                    }
                    p { class: "ik-muted", {i18n.t("verifyEmail.failed")} }
                    Link {
                        to: Route::Login {},
                        class: "ik-btn primary",
                        style: "width:100%;",
                        {i18n.t("common.backToSignIn")}
                    }
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A rejected sign-in must not be worded "You need to sign in to do that."
    ///
    /// That is the shared catalogue's sentence for *any* 401, and for a while it was what the
    /// sign-in form showed when the password was wrong — the one screen where it is
    /// meaningless, because the reader is already signing in and the sentence names nothing
    /// to correct. It cost a live debugging session: the message reads like a session or
    /// routing fault, so it sent the search towards the auth middleware and the proxy, when
    /// the API had in fact answered correctly and audited the attempt as `bad_password`.
    ///
    /// The generic wording stays right everywhere else, so the fix is per-screen and this
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
