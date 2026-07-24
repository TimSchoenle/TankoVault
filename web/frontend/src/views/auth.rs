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
    let mut busy = use_signal(|| false);
    let api_client = api::use_api();

    let submit = use_callback(move |()| {
        if *busy.read() {
            return;
        }
        busy.set(true);
        error.set(None);
        let is_register = *register_mode.read();
        let email_v = email.read().trim().to_owned();
        let username_v = username.read().trim().to_owned();
        let login_v = login.read().trim().to_owned();
        let password_v = password.read().clone();
        let client = api_client.clone();
        spawn(async move {
            let result = if is_register {
                client
                    .register()
                    .body(RegisterRequest {
                        email: email_v,
                        username: username_v,
                        password: password_v,
                    })
                    .send()
                    .await
            } else {
                client
                    .login()
                    .body(LoginRequest {
                        login: login_v,
                        password: password_v,
                    })
                    .send()
                    .await
            };
            match result {
                Ok(res) => {
                    let tok = res.into_inner();
                    session.set_token(tok.access_token);
                    nav.push(Route::Discover {});
                }
                Err(e) => error.set(Some(api::friendly_error(e))),
            }
            busy.set(false);
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
