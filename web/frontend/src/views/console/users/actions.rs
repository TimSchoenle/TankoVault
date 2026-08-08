//! The inspector's free-standing mutations.
//!
//! Free functions rather than closures on purpose: each needs every handle the editor holds,
//! and a closure that moves its captured signals into a spawned task can only be called once —
//! which matters when the header and a tab body both offer the same action.

use crate::api;
use crate::components::StepUpGate;
use crate::hooks::{use_busy, Reload};
use crate::i18n::use_i18n;
use crate::wire::types::DeleteUser;
use dioxus::prelude::*;

/// Erase an account.
#[expect(
    clippy::too_many_arguments,
    reason = "every parameter is a distinct handle type, so none can be transposed; grouping \
              them would only name the editor's whole state twice"
)]
pub(super) fn erase(
    api: api::Api,
    i18n: crate::i18n::Translator,
    busy: crate::hooks::Busy,
    outcome: Signal<crate::hooks::Outcome>,
    user_id: String,
    username: String,
    reason: String,
    reload: Reload,
    on_erased: EventHandler<()>,
    gate: StepUpGate,
) {
    if !busy.claim() {
        return;
    }
    let mut outcome = outcome;
    outcome.set(None);
    let client = gate.client(api);
    spawn(async move {
        let body = DeleteUser {
            // The server re-checks this against the account it is about to erase;
            // `TypeToConfirm` has already proved the operator typed it.
            confirm_username: username,
            reason: (!reason.is_empty()).then_some(reason),
        };
        match client.delete_user().id(user_id).body(body).send().await {
            Ok(_) => {
                on_erased.call(());
                reload.bump();
            }
            Err(e) => {
                if !gate.refused(api::Refusal::of(&e)) {
                    outcome.set(Some(Err(api::guarded_error(i18n, e))));
                }
            }
        }
        busy.release();
    });
}

/// Force an account out of every device it is signed in on.
pub(super) fn revoke_all(
    api: api::Api,
    i18n: crate::i18n::Translator,
    busy: crate::hooks::Busy,
    outcome: Signal<crate::hooks::Outcome>,
    detail_reload: Reload,
    user_id: String,
    gate: StepUpGate,
) {
    if !busy.claim() {
        return;
    }
    let mut outcome = outcome;
    outcome.set(None);
    let client = gate.client(api);
    spawn(async move {
        match client.revoke_user_sessions().id(user_id).send().await {
            Ok(_) => {
                outcome.set(Some(Ok(i18n.t("console.users.signedOutEverywhere"))));
                detail_reload.bump();
            }
            Err(e) => {
                if !gate.refused(api::Refusal::of(&e)) {
                    outcome.set(Some(Err(api::guarded_error(i18n, e))));
                }
            }
        }
        busy.release();
    });
}

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
        if !busy.claim() {
            return;
        }
        let id = user_id.clone();
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
                button { class: "ik-btn xs", disabled: busy.is_busy(), onclick: verify,
                    {i18n.t("console.users.verifyEmail")}
                }
            }
        }
    }
}
