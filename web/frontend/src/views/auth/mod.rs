//! Login / Register (§17.4 auth flow). Access token is held in memory; the refresh token
//! is set as an httpOnly cookie by the API. On success we route to Discover.

mod sign_in;

use sign_in::SignIn;

use crate::api;
use crate::components::Field;
use crate::i18n::{use_i18n, Translator};
use crate::icons::{Ic, Icon};
use crate::models::*;
use crate::state::capabilities::use_capabilities;
use crate::state::use_session;
use crate::views::DiscoverQuery;
use crate::webauthn::{self};
use crate::wire::types::Feature;
use crate::Route;
use dioxus::prelude::*;
use inkstone_ui::{button_class, Button, Size, Tone};

#[component]
pub(crate) fn Login() -> Element {
    let form = SignIn::use_new();
    let caps = use_capabilities();
    let submit = use_callback(move |()| form.submit());
    let finish_mfa = use_callback(move |()| form.finish_mfa());
    let passkey_sign_in = use_callback(move |()| form.passkey_sign_in());
    let resend = use_callback(move |()| form.resend());
    let SignIn {
        i18n,
        busy,
        mut register_mode,
        mut email,
        mut username,
        mut login,
        mut password,
        mut error,
        info,
        needs_verification,
        pending_mfa,
        ..
    } = form;

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
                Button {
                    style: "width:100%;margin:0 0 14px;",
                    on_click: move |_| resend.call(()),
                    {i18n.t("auth.resendConfirmation")}
                }
            }

            // The second leg takes over the whole form rather than appearing beside it: the
            // password has already been accepted, and leaving it on screen invites a reader who
            // mistypes their code to retype the password instead and wonder why nothing happens.
            if pending_mfa.read().is_some() {
                {second_factor_step(&form, finish_mfa)}
            } else if is_register {
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

            // Offered only where it can work: the deployment has the feature on, and the platform
            // can run a ceremony — `navigator.credentials` in a secure context on the web build,
            // Windows Hello on the desktop one. A button that can only report "not available
            // here" is worse than no button.
            if !is_register && caps.has_feature(Feature::AccountsPasskeys) && webauthn::is_available() {
                div { class: "ik-or", style: "margin:4px 0 12px;text-align:center;",
                    span { class: "ik-muted", style: "font-size:12px;", {i18n.t("common.or")} }
                }
                Button {
                    style: "width:100%;margin-bottom:14px;",
                    disabled: busy.is_busy(),
                    on_click: move |_| passkey_sign_in.call(()),
                    {i18n.t("passkey.signIn")}
                }
            }

            Button {
                tone: Tone::Primary,
                style: "width:100%;",
                disabled: busy.is_busy(),
                on_click: move |_| submit.call(()),
                if busy.is_busy() {
                {i18n.t("common.working")}
                } else {
                "{cta}"
                }
            }

            Button {
                style: "width:100%;margin-top:10px;",
                on_click: move |_| {
                    error.set(None);
                    let now = *register_mode.read();
                    register_mode.set(!now);
                },
                "{toggle_label}"
            }

            if is_register {
                {acceptance(i18n)}
            }
        }
    }
}

/// The second leg of a sign-in: the code or recovery code the password was not enough without.
fn second_factor_step(form: &SignIn, finish_mfa: Callback<()>) -> Element {
    let SignIn {
        i18n,
        busy,
        mut error,
        mut pending_mfa,
        mut second_factor,
        mut using_recovery,
        ..
    } = *form;
    rsx! {
        p { class: "ik-muted", style: "text-align:left;", {i18n.t("auth.mfa.intro")} }
        Field {
            id: "tv-auth-mfa",
            label: if *using_recovery.read() {
                i18n.t("auth.mfa.field.recovery")
            } else {
                i18n.t("auth.mfa.field.code")
            },
            autocomplete: if *using_recovery.read() { "off" } else { "one-time-code" },
            value: second_factor(),
            on_input: move |v| second_factor.set(v),
            on_enter: move |()| finish_mfa.call(()),
        }
        Button {
            tone: Tone::Primary,
            style: "width:100%;margin-top:10px;",
            disabled: busy.is_busy(),
            on_click: move |_| finish_mfa.call(()),
            if busy.is_busy() {
            {i18n.t("common.working")}
            } else {
            {i18n.t("auth.mfa.verify")}
            }
        }
        Button {
            style: "width:100%;margin-top:8px;",
            on_click: move |_| {
                let next = !*using_recovery.read();
                using_recovery.set(next);
                second_factor.set(String::new());
                error.set(None);
            },
            if *using_recovery.read() {
            {i18n.t("auth.mfa.useCode")}
            } else {
            {i18n.t("auth.mfa.useRecovery")}
            }
        }
        Button {
            style: "width:100%;margin-top:8px;",
            on_click: move |_| {
                // Abandoning the challenge leaves it to expire on its own; there is no
                // session to tear down, because none was issued.
                pending_mfa.set(None);
                second_factor.set(String::new());
                error.set(None);
            },
            {i18n.t("common.cancel")}
        }
    }
}

/// "By creating an account you accept the Terms of Service and the Data Policy."
///
/// The one place these two must be reachable *before* consent, which is why the API serves them
/// unauthenticated. Rendered **only if both are configured**: an operator who publishes neither
/// gets no sentence rather than one pointing nowhere, because a consent line that links a 404 is
/// worse than no line at all. The sentence is one catalogue string with a placeholder per link,
/// so a translation can reorder the clauses around them.
fn acceptance(i18n: Translator) -> Element {
    const SLUGS: [&str; 2] = ["terms", "privacy"];
    // Checked here as well as inside the sentence, so an unpublished pair leaves no empty
    // paragraph holding the sentence's margin.
    if SLUGS
        .iter()
        .any(|slug| terrace_legal_dioxus::use_published(slug).is_none())
    {
        return rsx! {};
    }
    rsx! {
        p { class: "ik-muted", style: "font-size:12px;line-height:1.6;margin:14px 0 0;text-align:center;",
            terrace_legal_dioxus::AcceptanceSentence {
                template: i18n.t("auth.acceptance"),
                slugs: SLUGS.map(str::to_owned).to_vec(),
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

    // `use_resource` re-runs only if `token` changes, so a stale link isn't retried every render.
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
            nav.push(Route::Discover {
                query: DiscoverQuery::default(),
            });
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
                        class: button_class(Tone::Primary, Size::Md, false),
                        style: "width:100%;",
                        {i18n.t("common.backToSignIn")}
                    }
                },
            }
        }
    }
}
