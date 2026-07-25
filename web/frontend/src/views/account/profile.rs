//! Profile panel — display name and email (`PATCH /v1/me/profile`).

use crate::api;
use crate::components::OutcomeLine;
use crate::hooks::{use_busy, use_outcome};
use crate::models::ProfileUpdate;
use crate::state::use_session;
use crate::util::initial;
use dioxus::prelude::*;

#[component]
pub(crate) fn ProfilePanel(name: String, role: &'static str) -> Element {
    let session = use_session();
    let api = api::use_api();
    let busy = use_busy();
    let mut outcome = use_outcome();
    let mut username = use_signal(|| name.clone());
    let mut email = use_signal(String::new);

    let save = move |_| {
        let new_username = username.peek().trim().to_owned();
        let new_email = email.peek().trim().to_owned();
        if new_username.is_empty() && new_email.is_empty() {
            outcome.set(Some(Err(
                "Enter a new display name or email first.".to_owned()
            )));
            return;
        }
        if !busy.claim() {
            return;
        }
        outcome.set(None);
        let client = api.client();
        spawn(async move {
            let update = ProfileUpdate {
                username: (!new_username.is_empty()).then_some(new_username),
                email: (!new_email.is_empty()).then_some(new_email),
            };
            match client.patch_profile().body(update).send().await {
                Ok(response) => {
                    let profile = response.into_inner();
                    // Reflect the server's canonical value immediately, both here and — via
                    // the session override — everywhere else the name appears. No relog.
                    username.set(profile.username.clone());
                    session.set_display_name(profile.username);
                    email.set(String::new());
                    outcome.set(Some(Ok("Profile updated.".to_owned())));
                }
                Err(e) => outcome.set(Some(Err(api::friendly_error(e)))),
            }
            busy.release();
        });
    };

    let current_name = username.read().clone();
    rsx! {
        div { class: "ik-sidebar-card", style: "max-width:560px;",
            div { class: "ik-flex", style: "margin-bottom:16px;",
                div { class: "ik-avatar", style: "width:56px;height:56px;font-size:22px;",
                    "{initial(&current_name)}"
                }
                div {
                    div { style: "font-family:var(--font-display);font-size:20px;font-weight:700;",
                        "{current_name}"
                    }
                    div { class: "ik-mono ik-muted", style: "font-size:12px;", "{role}" }
                }
            }
            div { class: "ik-field",
                label { r#for: "tv-profile-name", "Display name" }
                input {
                    id: "tv-profile-name",
                    class: "ik-input",
                    value: "{current_name}",
                    oninput: move |e| username.set(e.value()),
                }
            }
            div { class: "ik-field",
                label { r#for: "tv-profile-email", "Email" }
                input {
                    id: "tv-profile-email",
                    class: "ik-input",
                    r#type: "email",
                    placeholder: "new email address",
                    value: "{email}",
                    oninput: move |e| email.set(e.value()),
                }
            }
            OutcomeLine { outcome: outcome.read().clone() }
            button {
                class: "ik-btn primary",
                style: "margin-top:12px;",
                disabled: busy.is_busy(),
                onclick: save,
                if busy.is_busy() { "Saving…" } else { "Save profile" }
            }
        }
    }
}
