//! Security & sessions (§9.4) — list the caller's active login sessions and revoke any one
//! (its whole rotation family). Password change and 2FA have no endpoint yet, and the panel
//! says so rather than showing controls that would do nothing.

use crate::api;
use crate::components::{async_list, PanelCard};
use crate::hooks::{use_busy, use_reload, Reload};
use crate::i18n::use_i18n;
use crate::icons::Icon;
use crate::state::use_session;
use crate::util::iso_date;
use dioxus::prelude::*;
use progenitor_client::ResponseValue;

#[component]
pub(crate) fn SecurityPanel() -> Element {
    let session = use_session();
    let i18n = use_i18n();
    let api = api::use_api();
    let reload = use_reload();

    let sessions = use_resource(move || {
        reload.track();
        let client = api.client();
        let authed = session.is_authenticated();
        async move {
            if !authed {
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
        PanelCard { icon: Icon::ShieldLock, title: i18n.t("account.security.title"),
            p { class: "ik-muted", style: "font-size:13px;margin-top:0;",
                {i18n.t("account.security.intro")}
            }
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

#[component]
fn SessionRow(
    session_id: String,
    created_at: String,
    expires_at: String,
    reload: Reload,
) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let busy = use_busy();
    let created = iso_date(Some(&created_at)).to_owned();
    let expires = iso_date(Some(&expires_at)).to_owned();

    let revoke = move |_| {
        if !busy.claim() {
            return;
        }
        let id = session_id.clone();
        let client = api.client();
        spawn(async move {
            if client.delete_session().id(id).send().await.is_ok() {
                reload.bump();
            }
            busy.release();
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
