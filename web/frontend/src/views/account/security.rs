//! Security (§9.4) — two independent cards under one tab.
//!
//! - **Passkeys** ([`super::passkeys::PasskeysCard`]): register, rename and revoke `WebAuthn`
//!   credentials.
//! - **Sessions** (here): list the caller's active login sessions and revoke any one — its
//!   whole rotation family, not the token on screen, which would sign them out for exactly one
//!   request cycle.
//!
//! Each is behind its own feature flag; the tab shows whichever the deployment offers. Password
//! change still has no screen.

use crate::api;
use crate::components::{async_list, use_step_up_gate, PanelCard, StepUpGate, StepUpGuard};
use crate::hooks::{use_busy, use_reload, Reload};
use crate::i18n::use_i18n;
use crate::icons::Icon;
use crate::state::capabilities::use_capabilities;
use crate::state::use_session;
use crate::util::iso_date;
use crate::wire::types::Feature;
use dioxus::prelude::*;
use progenitor_client::ResponseValue;

#[component]
pub(crate) fn SecurityPanel() -> Element {
    let session = use_session();
    let i18n = use_i18n();
    let api = api::use_api();
    let caps = use_capabilities();
    let gate = use_step_up_gate();
    let reload = use_reload();

    // Read once, above the resource: skips a fetch to a route that doesn't exist here, which
    // would otherwise render an error box for a feature already decided not to show.
    let sessions_enabled = caps.has_feature(Feature::AccountsSessions);

    let sessions = use_resource(move || {
        reload.track();
        let client = api.client();
        let fetch = session.is_authenticated() && sessions_enabled;
        async move {
            if !fetch {
                return Ok(Vec::new());
            }
            client
                .sessions()
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });

    rsx! {
        // Two-factor first: it is the prerequisite for a passkey, so a reader who arrives
        // wanting one meets the thing they need before the thing they came for.
        if caps.has_feature(Feature::AccountsMfa) {
            super::mfa::MfaCard {}
        }

        if caps.has_feature(Feature::AccountsPasskeys) {
            super::passkeys::PasskeysCard {}
        }

        if sessions_enabled {
        PanelCard { icon: Icon::ShieldLock, title: i18n.t("account.security.title"),
            p { class: "ik-muted", style: "font-size:13px;margin-top:0;",
                {i18n.t("account.security.intro")}
            }
            // Revoking a session is behind a step-up, so a refusal on any row opens this one
            // prompt. Without it the button simply did nothing, which reads as a broken control.
            StepUpGuard { gate }
            {
                async_list(
                    &sessions,
                    reload,
                    || rsx! { crate::components::SkeletonBlock { height: 80 } },
                    &i18n.t("account.security.empty"),
                    |rows| rsx! {
                        for row in rows.iter().cloned() {
                            SessionRow {
                                key: "{row.id}",
                                session_id: row.id,
                                created_at: row.created_at,
                                expires_at: row.expires_at,
                                reload,
                                gate,
                            }
                        }
                    },
                )
            }
            p { class: "ik-muted", style: "font-size:12px;margin-top:14px;",
                {i18n.t("account.security.unavailable")}
            }
        }
        }
    }
}

/// One active session, with the revoke that ends its whole rotation family.
///
/// `gate` belongs to the card: a refusal has to open the one prompt it renders rather than a
/// prompt per row.
#[component]
fn SessionRow(
    session_id: String,
    created_at: String,
    expires_at: String,
    reload: Reload,
    gate: StepUpGate,
) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let busy = use_busy();
    let created = iso_date(Some(&created_at)).to_owned();
    let expires = iso_date(Some(&expires_at)).to_owned();

    let revoke = move |_| {
        let id = session_id.clone();
        gate.attempt(move || {
            if !busy.claim() {
                return;
            }
            let id = id.clone();
            let client = gate.client(api);
            spawn(async move {
                match client.delete_session().id(id).send().await {
                    Ok(_) => reload.bump(),
                    // The row has no error line of its own; a `403` opens the card's prompt, and
                    // anything else leaves the list as it was, as it did before the gate existed.
                    Err(e) => {
                        let _refused = gate.refused(api::Refusal::of(&e));
                    }
                }
                busy.release();
            });
        });
    };

    rsx! {
        div { class: "ik-row",
            div { class: "grow",
                div { style: "font-weight:600;font-size:13px;",
                    {i18n.args("account.security.signedIn", &[("date", &created)])}
                }
                div { class: "ik-mono ik-muted", style: "font-size:11px;",
                    {i18n.args("account.security.expires", &[("date", &expires)])}
                }
            }
            button { class: "ik-btn", disabled: busy.is_busy(), onclick: revoke,
                if busy.is_busy() {
                    {i18n.t("account.security.revoking")}
                } else {
                    {i18n.t("account.security.revoke")}
                }
            }
        }
    }
}
