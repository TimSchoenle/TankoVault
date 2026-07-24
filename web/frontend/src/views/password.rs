//! Password-reset flow (§17.4): request a reset email (`/forgot-password`) and choose a new
//! password from the emailed link (`/reset-password?token=…`). Both screens reuse the sign-in
//! card styling and the shared [`AuthBrand`](super::auth::AuthBrand) lockup.

use super::auth::AuthBrand;
use crate::api;
use crate::models::*;
use crate::Route;
use dioxus::prelude::*;

/// Request a password-reset email. The API always answers `202 Accepted` whether or not the
/// address is registered (so it can't be used to probe accounts), so the UI shows the same
/// reassuring confirmation regardless — the only failure surfaced is a transport error.
#[component]
pub fn ForgotPassword() -> Element {
    let mut email = use_signal(String::new);
    let mut error = use_signal(|| Option::<String>::None);
    let mut sent = use_signal(|| false);
    let mut busy = use_signal(|| false);
    let api_client = api::use_api();

    let submit = use_callback(move |()| {
        if *busy.read() {
            return;
        }
        let email_v = email.read().trim().to_owned();
        if email_v.is_empty() {
            error.set(Some("Enter your email address.".to_owned()));
            return;
        }
        busy.set(true);
        error.set(None);
        let client = api_client.clone();
        spawn(async move {
            match client
                .forgot_password()
                .body(ForgotPasswordRequest { email: email_v })
                .send()
                .await
            {
                Ok(_) => sent.set(true),
                Err(e) => error.set(Some(api::friendly_error(e))),
            }
            busy.set(false);
        });
    });

    rsx! {
        div { class: "ik-auth",
            AuthBrand {}
            h1 { "Reset your password" }

            if *sent.read() {
                p { class: "ik-muted",
                    "If an account exists for that address, we've sent a link to reset your \
                     password. It expires in 1 hour."
                }
                Link {
                    to: Route::Login {},
                    class: "ik-btn primary",
                    style: "width:100%;margin-top:8px;",
                    "Back to sign in"
                }
            } else {
                p { class: "ik-muted",
                    "Enter your account's email address and we'll send you a link to choose a \
                     new password."
                }

                if let Some(msg) = error.read().clone() {
                    div { class: "ik-error", style: "padding:12px;margin:14px 0;text-align:left;",
                        "{msg}"
                    }
                }

                div { class: "ik-field",
                    label { "Email" }
                    input {
                        class: "ik-input",
                        r#type: "email",
                        value: "{email}",
                        oninput: move |e| email.set(e.value()),
                        onkeydown: move |e| {
                            if e.key() == Key::Enter {
                                submit.call(());
                            }
                        },
                    }
                }

                button {
                    class: "ik-btn primary",
                    style: "width:100%;",
                    disabled: *busy.read(),
                    onclick: move |_| submit.call(()),
                    if *busy.read() {
                        "Sending…"
                    } else {
                        "Send reset link"
                    }
                }

                Link {
                    to: Route::Login {},
                    class: "ik-btn",
                    style: "width:100%;margin-top:10px;",
                    "Back to sign in"
                }
            }
        }
    }
}

/// Choose a new password using the one-time token from the reset email
/// (`/reset-password?token=…`). Enforces the same minimum length as registration and confirms
/// the two entries match before calling the API, which additionally revokes existing sessions.
#[component]
pub fn ResetPassword(token: String) -> Element {
    let mut password = use_signal(String::new);
    let mut confirm = use_signal(String::new);
    let mut error = use_signal(|| Option::<String>::None);
    let mut done = use_signal(|| false);
    let mut busy = use_signal(|| false);
    let api_client = api::use_api();

    let submit = use_callback(move |()| {
        if *busy.read() {
            return;
        }
        let password_v = password.read().clone();
        let confirm_v = confirm.read().clone();
        if password_v.len() < 8 {
            error.set(Some("Password must be at least 8 characters.".to_owned()));
            return;
        }
        if password_v != confirm_v {
            error.set(Some("Passwords don't match.".to_owned()));
            return;
        }
        busy.set(true);
        error.set(None);
        let client = api_client.clone();
        let token_v = token.clone();
        spawn(async move {
            match client
                .reset_password()
                .body(ResetPasswordRequest {
                    token: token_v,
                    new_password: password_v,
                })
                .send()
                .await
            {
                Ok(_) => done.set(true),
                // A 400 here means the token itself is bad (invalid, expired, or already
                // used) rather than the new password, so name that specifically.
                Err(e) if api::error_status(&e) == Some(400) => error.set(Some(
                    "This reset link is invalid or has expired. Request a new one from the \
                     \"Forgot your password?\" page."
                        .to_owned(),
                )),
                Err(e) => error.set(Some(api::friendly_error(e))),
            }
            busy.set(false);
        });
    });

    rsx! {
        div { class: "ik-auth",
            AuthBrand {}
            h1 { "Choose a new password" }

            if *done.read() {
                p { class: "ik-muted",
                    "Your password has been changed and any other active sessions were signed \
                     out. You can sign in with your new password now."
                }
                Link {
                    to: Route::Login {},
                    class: "ik-btn primary",
                    style: "width:100%;margin-top:8px;",
                    "Sign in"
                }
            } else {
                if let Some(msg) = error.read().clone() {
                    div { class: "ik-error", style: "padding:12px;margin:14px 0;text-align:left;",
                        "{msg}"
                    }
                }

                div { class: "ik-field",
                    label { "New password" }
                    input {
                        class: "ik-input",
                        r#type: "password",
                        value: "{password}",
                        oninput: move |e| password.set(e.value()),
                    }
                }
                div { class: "ik-field",
                    label { "Confirm new password" }
                    input {
                        class: "ik-input",
                        r#type: "password",
                        value: "{confirm}",
                        oninput: move |e| confirm.set(e.value()),
                        onkeydown: move |e| {
                            if e.key() == Key::Enter {
                                submit.call(());
                            }
                        },
                    }
                }

                button {
                    class: "ik-btn primary",
                    style: "width:100%;",
                    disabled: *busy.read(),
                    onclick: move |_| submit.call(()),
                    if *busy.read() {
                        "Saving…"
                    } else {
                        "Change password"
                    }
                }

                Link {
                    to: Route::Login {},
                    class: "ik-btn",
                    style: "width:100%;margin-top:10px;",
                    "Back to sign in"
                }
            }
        }
    }
}
