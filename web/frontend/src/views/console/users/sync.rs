//! This account's linked external trackers, and the two admin-side actions the API supports.

use crate::api;
use crate::components::{async_view, ErrorLine, InlineConfirm, Section, SkeletonBlock};
use crate::hooks::{use_busy, use_reload, Reload};
use crate::i18n::use_i18n;
use crate::models::AdminSyncAccount;
use crate::util::{monogram, rel_time};
use crate::wire::types::{SyncAccountTarget, UserId};
use dioxus::prelude::*;
use progenitor_client::ResponseValue;

/// This account's linked external trackers, with the two admin-side actions the API supports.
#[component]
pub(super) fn ExternalSync(user_id: String, username: String, editable: bool) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let reload = use_reload();
    let Ok(uuid) = uuid::Uuid::parse_str(&user_id) else {
        return rsx! {};
    };

    let accounts = use_resource(move || {
        reload.track();
        let client = api.client();
        async move {
            client
                .list_sync_accounts()
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });

    rsx! {
        Section { label: i18n.t("console.users.externalSync"),
            {
                async_view(
                    &accounts,
                    reload,
                    || rsx! { SkeletonBlock { height: 90 } },
                    |all| {
                        let mine: Vec<AdminSyncAccount> = all
                            .iter()
                            .filter(|row| row.user_id == uuid)
                            .cloned()
                            .collect();
                        if mine.is_empty() {
                            return rsx! {
                                p { class: "ik-muted", style: "font-size:12px;margin:0;",
                                    {i18n.t("console.users.noSyncLinks")}
                                }
                            };
                        }
                        rsx! {
                            div { class: "ik-listbox",
                                for account in mine {
                                    SyncLinkRow {
                                        key: "{account.provider}",
                                        account,
                                        username: username.clone(),
                                        editable,
                                        reload,
                                    }
                                }
                            }
                        }
                    },
                )
            }
        }
    }
}

/// One linked tracker: force a pull, or unlink it behind an inline confirmation. Unlinking is
/// reversible — the reader can relink themselves — so it asks once rather than demanding typing.
#[component]
pub(super) fn SyncLinkRow(
    account: AdminSyncAccount,
    username: String,
    editable: bool,
    reload: Reload,
) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let busy = use_busy();
    let mut confirming = use_signal(|| false);
    let mut error = use_signal(|| Option::<String>::None);

    let target = SyncAccountTarget {
        provider: account.provider.clone(),
        user_id: UserId(account.user_id),
    };
    let external = account
        .external_username
        .clone()
        .unwrap_or_else(|| username.clone());

    let pull = {
        let target = target.clone();
        move |_| {
            if !busy.claim() {
                return;
            }
            error.set(None);
            let target = target.clone();
            let client = api.client();
            spawn(async move {
                if let Err(e) = client.admin_sync_pull().body(target).send().await {
                    error.set(Some(api::friendly_error(i18n, e)));
                }
                reload.bump();
                busy.release();
            });
        }
    };

    let unlink = {
        let target = target.clone();
        move |()| {
            if !busy.claim() {
                return;
            }
            error.set(None);
            let target = target.clone();
            let client = api.client();
            spawn(async move {
                match client.admin_sync_unlink().body(target).send().await {
                    Ok(_) => {
                        confirming.set(false);
                        reload.bump();
                    }
                    Err(e) => error.set(Some(api::friendly_error(i18n, e))),
                }
                busy.release();
            });
        }
    };

    if *confirming.read() {
        return rsx! {
            InlineConfirm {
                title: i18n.args("console.users.unlinkTitle", &[("provider", &account.provider)]),
                body: i18n.t("console.users.unlinkWhy"),
                cta: i18n.t("console.users.unlink"),
                busy: busy.is_busy(),
                on_cancel: move |()| confirming.set(false),
                on_confirm: unlink,
            }
        };
    }

    rsx! {
        div { class: "ik-listrow",
            span { class: "ik-mono-tile lg jade", {monogram(&account.provider)} }
            div { style: "min-width:0;",
                div { style: "font-weight:600;font-size:12.5px;", "{account.provider} · {external}" }
                div { class: "ik-mono", style: "font-size:10.5px;color:var(--muted);margin-top:1px;",
                    {
                        i18n.args(
                            "console.users.syncMeta",
                            &[
                                ("when", &rel_time(i18n, account.last_synced_at.as_deref())),
                                ("conflicts", &account.pending_conflicts.to_string()),
                            ],
                        )
                    }
                }
                if let Some(message) = error.read().clone() {
                    ErrorLine { message }
                }
            }
            if editable {
                div { class: "ik-flex", style: "margin-left:auto;gap:6px;flex:none;",
                    button { class: "ik-btn xs", disabled: busy.is_busy(), onclick: pull,
                        {i18n.t("console.users.forcePull")}
                    }
                    button {
                        class: "ik-btn xs acc",
                        disabled: busy.is_busy(),
                        onclick: move |_| confirming.set(true),
                        {i18n.t("console.users.unlink")}
                    }
                }
            }
        }
    }
}
