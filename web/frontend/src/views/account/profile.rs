//! Profile panel — display name and email (`PATCH /v1/me/profile`).

use crate::api;
use crate::components::OutcomeLine;
use crate::hooks::{use_busy, use_outcome};
use crate::i18n::use_i18n;
use crate::models::ProfileUpdate;
use crate::state::use_session;
use crate::util::initial;
use dioxus::prelude::*;

#[component]
pub(crate) fn ProfilePanel(name: String, tier: String) -> Element {
    let session = use_session();
    let i18n = use_i18n();
    let api = api::use_api();
    let busy = use_busy();
    let mut outcome = use_outcome();
    let mut username = use_signal(|| name.clone());
    let mut email = use_signal(String::new);

    let save = move |_| {
        let new_username = username.peek().trim().to_owned();
        let new_email = email.peek().trim().to_owned();
        if new_username.is_empty() && new_email.is_empty() {
            outcome.set(Some(Err(i18n.t("account.profile.nothingToSave"))));
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
                    outcome.set(Some(Ok(i18n.t("account.profile.updated"))));
                }
                Err(e) => outcome.set(Some(Err(api::friendly_error(i18n, e)))),
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
                    div { class: "ik-mono ik-muted", style: "font-size:12px;", "{tier}" }
                }
            }
            div { class: "ik-field",
                label { r#for: "tv-profile-name", {i18n.t("account.profile.displayName")} }
                input {
                    id: "tv-profile-name",
                    class: "ik-input",
                    value: "{current_name}",
                    oninput: move |e| username.set(e.value()),
                }
            }
            div { class: "ik-field",
                label { r#for: "tv-profile-email", {i18n.t("auth.field.email")} }
                input {
                    id: "tv-profile-email",
                    class: "ik-input",
                    r#type: "email",
                    placeholder: i18n.t("account.profile.emailPlaceholder"),
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
                if busy.is_busy() {
                    {i18n.t("common.saving")}
                } else {
                    {i18n.t("account.profile.save")}
                }
            }
        }
    }
}
