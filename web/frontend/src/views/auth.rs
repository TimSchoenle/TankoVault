//! Login / Register (§17.4 auth flow). Access token is held in memory; the refresh token
//! is set as an httpOnly cookie by the API. On success we route to Discover.

use crate::api;
use crate::icons::{Ic, Icon};
use crate::models::*;
use crate::state::use_session;
use crate::Route;
use dioxus::prelude::*;

#[component]
pub fn Login() -> Element {
    let session = use_session();
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
    let mut busy = use_signal(|| false);
    let api_client = api::use_api();
    // A second clone for the resend callback below (the first is moved into `submit`).
    let resend_client = api_client.clone();

    let submit = use_callback(move |()| {
        if *busy.read() {
            return;
        }
        busy.set(true);
        error.set(None);
        info.set(None);
        needs_verification.set(false);
        let is_register = *register_mode.read();
        let email_v = email.read().trim().to_owned();
        let username_v = username.read().trim().to_owned();
        let login_v = login.read().trim().to_owned();
        let password_v = password.read().clone();
        let client = api_client.clone();
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
                            info.set(Some(
                                "Account created. We've emailed you a confirmation link — \
                                 click it to activate your account, then sign in."
                                    .to_owned(),
                            ));
                            register_mode.set(false);
                            password.set(String::new());
                        } else if let Some(token) = body.access_token {
                            // No mailer: the account was activated immediately.
                            session.set_token(token);
                            nav.push(Route::Discover {});
                        }
                    }
                    Err(e) => error.set(Some(api::friendly_error(e))),
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
                        error.set(Some(
                            "Please confirm your email address before signing in. Check your \
                             inbox for the confirmation link."
                                .to_owned(),
                        ));
                    }
                    Err(e) => error.set(Some(api::friendly_error(e))),
                }
            }
            busy.set(false);
        });
    });

    // Resend the confirmation link to the address entered in the sign-in field. Always
    // reports success (the endpoint is deliberately silent about whether the account exists).
    let resend = use_callback(move |()| {
        let email_v = login.read().trim().to_owned();
        if email_v.is_empty() {
            return;
        }
        let client = resend_client.clone();
        spawn(async move {
            let _ = client
                .resend_verification()
                .body(ResendVerificationRequest { email: email_v })
                .send()
                .await;
            needs_verification.set(false);
            error.set(None);
            info.set(Some(
                "If that address needs confirming, a new link is on its way.".to_owned(),
            ));
        });
    });

    let is_register = *register_mode.read();
    let heading = if is_register {
        "Create your account"
    } else {
        "Welcome back"
    };
    let cta = if is_register {
        "Create account"
    } else {
        "Sign in"
    };
    let toggle_label = if is_register {
        "Have an account? Sign in"
    } else {
        "New here? Create an account"
    };

    let subtitle = if is_register {
        "Create an account to sync your library across every device."
    } else {
        "Sign in to sync your library."
    };

    rsx! {
        div { class: "ik-auth",
            // Wordmark lockup (§7.9): gradient tile + TankōVault + tagline.
            div { class: "ik-auth-brand",
                div { class: "ik-brand-tile", Ic { icon: Icon::MenuBook, size: 22 } }
                div {
                    div { class: "ik-wordmark",
                        "Tankō"
                        span { class: "acc", "Vault" }
                    }
                    div { class: "ik-brand-tag", "SOURCE · TRACK · SYNC" }
                }
            }
            h1 { "{heading}" }
            p { class: "ik-muted", "{subtitle}" }

            if let Some(msg) = error.read().clone() {
                div { class: "ik-error", style: "padding:12px;margin:14px 0;text-align:left;",
                    "{msg}"
                }
            }

            if let Some(msg) = info.read().clone() {
                div {
                    class: "ik-note",
                    style: "padding:12px;margin:14px 0;text-align:left;border:1px solid var(--ik-border);border-radius:8px;",
                    "{msg}"
                }
            }

            if *needs_verification.read() {
                button {
                    class: "ik-btn",
                    style: "width:100%;margin:0 0 14px;",
                    r#type: "button",
                    onclick: move |_| resend.call(()),
                    "Resend confirmation email"
                }
            }

            if is_register {
                div { class: "ik-field",
                    label { "Email" }
                    input {
                        class: "ik-input",
                        r#type: "email",
                        value: "{email}",
                        oninput: move |e| email.set(e.value()),
                    }
                }
                div { class: "ik-field",
                    label { "Username" }
                    input {
                        class: "ik-input",
                        value: "{username}",
                        oninput: move |e| username.set(e.value()),
                    }
                }
            } else {
                div { class: "ik-field",
                    label { "Email or username" }
                    input {
                        class: "ik-input",
                        value: "{login}",
                        oninput: move |e| login.set(e.value()),
                    }
                }
            }
            div { class: "ik-field",
                label { "Password" }
                input {
                    class: "ik-input",
                    r#type: "password",
                    value: "{password}",
                    oninput: move |e| password.set(e.value()),
                    onkeydown: move |e| {
                        if e.key() == Key::Enter {
                            submit.call(());
                        }
                    },
                }
            }

            if !is_register {
                div { style: "text-align:right;margin:-4px 0 14px;",
                    Link {
                        to: Route::ForgotPassword {},
                        class: "ik-link",
                        "Forgot your password?"
                    }
                }
            }

            button {
                class: "ik-btn primary",
                style: "width:100%;",
                disabled: *busy.read(),
                onclick: move |_| submit.call(()),
                if *busy.read() {
                    "Working…"
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
pub fn AuthBrand() -> Element {
    rsx! {
        div { class: "ik-auth-brand",
            div { class: "ik-brand-tile", Ic { icon: Icon::MenuBook, size: 22 } }
            div {
                div { class: "ik-wordmark",
                    "Tankō"
                    span { class: "acc", "Vault" }
                }
                div { class: "ik-brand-tag", "SOURCE · TRACK · SYNC" }
            }
        }
    }
}

/// Email-confirmation landing page for the link in the sign-up email
/// (`/verify-email?token=…`). Confirms the token on mount and, on success, adopts the issued
/// session and drops the user straight into the app.
#[component]
pub fn VerifyEmail(token: String) -> Element {
    let session = use_session();
    let nav = use_navigator();
    let api_client = api::use_api();

    // Fire the confirmation once for this token; `use_resource` re-runs only if `token`
    // changes, so a stale link isn't retried on every render.
    let token_for_call = token.clone();
    let resource = use_resource(move || {
        let client = api_client.clone();
        let token = token_for_call.clone();
        async move {
            client
                .verify_email()
                .body(VerifyEmailRequest { token })
                .send()
                .await
                .map(|r| r.into_inner().access_token)
                .map_err(api::friendly_error)
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
            h1 { "Confirm your email" }
            match state {
                None => rsx! {
                    p { class: "ik-muted", "Confirming your email address…" }
                },
                Some(Ok(_)) => rsx! {
                    p { class: "ik-muted", "Email confirmed. Taking you to your library…" }
                },
                Some(Err(msg)) => rsx! {
                    div { class: "ik-error", style: "padding:12px;margin:14px 0;text-align:left;",
                        "{msg}"
                    }
                    p { class: "ik-muted",
                        "This confirmation link may have expired or already been used. Sign in \
                         to request a new one."
                    }
                    Link {
                        to: Route::Login {},
                        class: "ik-btn primary",
                        style: "width:100%;",
                        "Back to sign in"
                    }
                },
            }
        }
    }
}
