//! Confirming an account's address on its behalf.

use crate::api;
use crate::components::StepUpGate;
use crate::hooks::{use_busy, Reload};
use crate::i18n::use_i18n;
use dioxus::prelude::*;
use inkstone_ui::{Button, Size};

/// Confirm an address administratively, for an account that never clicked its link.
#[component]
pub(super) fn VerifyEmailAction(
    user_id: String,
    enabled: bool,
    reload: Reload,
    detail_reload: Reload,
    gate: StepUpGate,
) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let busy = use_busy();

    let verify = move |_| {
        let id = user_id.clone();
        gate.attempt(move || {
            if !busy.claim() {
                return;
            }
            let id = id.clone();
            let client = gate.client(api);
            spawn(async move {
                match client.verify_user_email().id(id).send().await {
                    Ok(_) => {
                        detail_reload.bump();
                        reload.bump();
                    }
                    // This control has no error line of its own; the refetch is what reports every
                    // other failure. An elevation demand changes nothing to refetch, so it is the
                    // one outcome that has to be raised to the editor's prompt.
                    Err(e) => {
                        let _refused = gate.refused(api::Refusal::of(&e));
                    }
                }
                busy.release();
            });
        });
    };

    rsx! {
        div { class: "ik-flex", style: "gap:8px;",
            span {
                class: "ik-mono",
                style: "font-size:12px;color:var(--star);",
                title: i18n.t("console.users.unverifiedHint"),
                {i18n.t("console.users.unverified")}
            }
            if enabled {
                Button {
                    size: Size::Xs,
                    disabled: busy.is_busy(),
                    on_click: verify,
                    {i18n.t("console.users.verifyEmail")}
                }
            }
        }
    }
}
