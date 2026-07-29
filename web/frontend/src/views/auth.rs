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
                    // A 403 on sign-in means the password was right but the email address
                    // hasn't been confirmed yet — offer to resend the link.
                    Err(e) if api::error_status(&e) == Some(403) => {
                        needs_verification.set(true);
                        error.set(Some(i18n.t("auth.confirmFirst")));
                    }
                    Err(e) => error.set(Some(api::friendly_error(i18n, e))),
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
