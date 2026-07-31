//! Password-reset flow (§17.4): request a reset email (`/forgot-password`) and choose a new
//! password from the emailed link (`/reset-password?token=…`). Both screens reuse the sign-in
//! card styling and the shared [`AuthBrand`](super::auth::AuthBrand) lockup.

use super::auth::AuthBrand;
use crate::api;
use crate::components::Field;
use crate::hooks::use_busy;
use crate::i18n::use_i18n;
use crate::models::*;
use crate::Route;
use dioxus::prelude::*;

/// Request a password-reset email. The API always answers `202 Accepted` whether or not the
/// address is registered (so it can't be used to probe accounts), so the UI shows the same
/// reassuring confirmation regardless — the only failure surfaced is a transport error.
#[component]
pub(crate) fn ForgotPassword() -> Element {
    let i18n = use_i18n();
    let mut email = use_signal(String::new);
    let mut error = use_signal(|| Option::<String>::None);
    let mut sent = use_signal(|| false);
    let busy = use_busy();
    let api = api::use_api();

    let submit = use_callback(move |()| {
        let email_v = email.read().trim().to_owned();
        if email_v.is_empty() {
            error.set(Some(i18n.t("password.forgot.emailRequired")));
            return;
        }
        if !busy.claim() {
            return;
        }
        error.set(None);
        let client = api.client();
        spawn(async move {
            match client
                .forgot_password()
                .body(ForgotPasswordRequest { email: email_v })
                .send()
                .await
            {
                Ok(_) => sent.set(true),
                Err(e) => error.set(Some(api::friendly_error(i18n, e))),
            }
            busy.release();
        });
    });

    rsx! {
        div { class: "ik-auth",
            AuthBrand {}
            h1 { {i18n.t("password.forgot.heading")} }

            if *sent.read() {
                p { class: "ik-muted", {i18n.t("password.forgot.sent")} }
                Link {
                    to: Route::Login {},
                    class: "ik-btn primary",
                    style: "width:100%;margin-top:8px;",
                    {i18n.t("common.backToSignIn")}
                }
            } else {
                p { class: "ik-muted", {i18n.t("password.forgot.intro")} }

                if let Some(msg) = error.read().clone() {
                    div { class: "ik-error", style: "padding:12px;margin:14px 0;text-align:left;",
                        "{msg}"
                    }
                }

                Field {
                    id: "tv-forgot-email",
                    label: i18n.t("auth.field.email"),
                    kind: "email",
                    autocomplete: "email",
                    value: email(),
                    on_input: move |v| email.set(v),
                    on_enter: move |()| submit.call(()),
                }

                button {
                    class: "ik-btn primary",
                    style: "width:100%;",
                    disabled: busy.is_busy(),
                    onclick: move |_| submit.call(()),
                    if busy.is_busy() {
                        {i18n.t("password.forgot.sending")}
                    } else {
                        {i18n.t("password.forgot.submit")}
                    }
                }

                Link {
                    to: Route::Login {},
                    class: "ik-btn",
                    style: "width:100%;margin-top:10px;",
                    {i18n.t("common.backToSignIn")}
                }
            }
        }
    }
}

/// Choose a new password using the one-time token from the reset email
/// (`/reset-password?token=…`). Enforces the same minimum length as registration and confirms
/// the two entries match before calling the API, which additionally revokes existing sessions.
#[component]
pub(crate) fn ResetPassword(token: String) -> Element {
    let i18n = use_i18n();
    let mut password = use_signal(String::new);
    let mut confirm = use_signal(String::new);
    let mut error = use_signal(|| Option::<String>::None);
    let mut done = use_signal(|| false);
    let busy = use_busy();
    let api = api::use_api();

    let submit = use_callback(move |()| {
        let password_v = password.read().clone();
        let confirm_v = confirm.read().clone();
        if password_v.len() < 8 {
            error.set(Some(i18n.t("password.tooShort")));
            return;
        }
        if password_v != confirm_v {
            error.set(Some(i18n.t("password.mismatch")));
            return;
        }
        if !busy.claim() {
            return;
        }
        error.set(None);
        let client = api.client();
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
                Err(e) if api::error_status(&e) == Some(400) => {
                    error.set(Some(i18n.t("password.reset.badToken")));
                }
                Err(e) => error.set(Some(api::friendly_error(i18n, e))),
            }
            busy.release();
        });
    });

    rsx! {
        div { class: "ik-auth",
            AuthBrand {}
            h1 { {i18n.t("password.reset.heading")} }

            if *done.read() {
                p { class: "ik-muted", {i18n.t("password.reset.done")} }
                Link {
                    to: Route::Login {},
                    class: "ik-btn primary",
                    style: "width:100%;margin-top:8px;",
                    {i18n.t("common.signIn")}
                }
            } else {
                if let Some(msg) = error.read().clone() {
                    div { class: "ik-error", style: "padding:12px;margin:14px 0;text-align:left;",
                        "{msg}"
                    }
                }

                Field {
                    id: "tv-reset-password",
                    label: i18n.t("password.reset.newPassword"),
                    kind: "password",
                    autocomplete: "new-password",
                    value: password(),
                    on_input: move |v| password.set(v),
                    on_enter: move |()| submit.call(()),
                }
                Field {
                    id: "tv-reset-confirm",
                    label: i18n.t("password.reset.confirmPassword"),
                    kind: "password",
                    autocomplete: "new-password",
                    value: confirm(),
                    on_input: move |v| confirm.set(v),
                    on_enter: move |()| submit.call(()),
                }

                button {
                    class: "ik-btn primary",
                    style: "width:100%;",
                    disabled: busy.is_busy(),
                    onclick: move |_| submit.call(()),
                    if busy.is_busy() {
                        {i18n.t("common.saving")}
                    } else {
                        {i18n.t("password.reset.submit")}
                    }
                }

                Link {
                    to: Route::Login {},
                    class: "ik-btn",
                    style: "width:100%;margin-top:10px;",
                    {i18n.t("common.backToSignIn")}
                }
            }
        }
    }
}
