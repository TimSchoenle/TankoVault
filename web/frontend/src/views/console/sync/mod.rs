//! Sync administration: the linked-account directory and the per-series mapping tools.

mod inspector;
mod queues;

use crate::api;
use crate::components::ErrorBox;
use crate::hooks::{use_reload, Reload};
use crate::i18n::use_i18n;
use crate::models::*;
use crate::util::rel_time;
use dioxus::prelude::*;
use inspector::SeriesSyncInspector;
use progenitor_client::ResponseValue;
use queues::AssignQueue;
use queues::UnmatchedRemoteQueue;

#[component]
pub(super) fn SyncAdminPanel() -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let reload = use_reload();
    // The series currently open in the "manga info" inspector, shared with the assign queue
    // so "Inspect" jumps straight to the editable per-provider mapping view.
    let selected = use_signal(|| Option::<String>::None);

    let accounts = {
        use_resource(move || {
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
        })
    };

    let accounts_body = match &*accounts.read_unchecked() {
        None => rsx! { div { class: "ik-skeleton", style: "height:60px;" } },
        Some(Err(e)) => {
            let msg = e.clone();
            rsx! { ErrorBox { message: msg, on_retry: move |()| reload.bump() } }
        }
        Some(Ok(list)) if list.is_empty() => rsx! {
            div { class: "ik-empty", {i18n.t("console.sync.noAccounts")} }
        },
        Some(Ok(list)) => {
            let list = list.clone();
            rsx! {
                for a in list {
                    SyncAccountRow { key: "{a.user_id}-{a.provider}", account: Signal::new(a), reload }
                }
            }
        }
    };

    rsx! {
        section { style: "margin-bottom:24px;",
            h3 { {i18n.t("console.sync.accounts")} }
            {accounts_body}
        }
        section { style: "margin-bottom:24px;",
            h3 { {i18n.t("console.sync.inspector")} }
            p { class: "ik-muted", style: "font-size:13px;margin-top:0;",
                {i18n.t("console.sync.inspectorIntro")}
            }
            SeriesSyncInspector { selected, reload }
        }
        section { style: "margin-bottom:24px;",
            h3 { {i18n.t("console.sync.assignQueue")} }
            p { class: "ik-muted", style: "font-size:13px;margin-top:0;",
                {i18n.t("console.sync.assignQueueIntro")}
            }
            AssignQueue { selected, reload }
        }
        section {
            h3 { {i18n.t("console.sync.remoteQueue")} }
            p { class: "ik-muted", style: "font-size:13px;margin-top:0;",
                {i18n.t("console.sync.remoteQueueIntro")}
            }
            UnmatchedRemoteQueue { reload }
        }
    }
}

#[component]
pub(super) fn SyncAccountRow(account: Signal<AdminSyncAccount>, reload: Reload) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let mut busy = use_signal(|| false);
    let acc = account.read();
    let last_sync = rel_time(i18n, acc.last_synced_at.as_deref());

    let pull = {
        let user_id = UserId(acc.user_id);
        let provider = acc.provider.clone();
        let _client = api.client();
        move |_| {
            if *busy.peek() {
                return;
            }
            busy.set(true);
            let provider = provider.clone();
            spawn(async move {
                let client = api.client();
                let body = tankovault_api_client::types::SyncAccountTarget { provider, user_id };
                if client.admin_sync_pull().body(body).send().await.is_ok() {
                    reload.bump();
                }
                busy.set(false);
            });
        }
    };

    let push = {
        let user_id = UserId(acc.user_id);
        let provider = acc.provider.clone();
        let _client = api.client();
        move |_| {
            if *busy.peek() {
                return;
            }
            busy.set(true);
            let provider = provider.clone();
            spawn(async move {
                let client = api.client();
                let body = tankovault_api_client::types::SyncAccountTarget { provider, user_id };
                if client.admin_sync_push().body(body).send().await.is_ok() {
                    reload.bump();
                }
                busy.set(false);
            });
        }
    };

    let unlink = {
        let user_id = UserId(acc.user_id);
        let provider = acc.provider.clone();
        let _client = api.client();
        move |_| {
            if *busy.peek() {
                return;
            }
            busy.set(true);
            let provider = provider.clone();
            spawn(async move {
                let client = api.client();
                let body = tankovault_api_client::types::SyncAccountTarget { provider, user_id };
                if client.admin_sync_unlink().body(body).send().await.is_ok() {
                    reload.bump();
                }
                busy.set(false);
            });
        }
    };

    rsx! {
        div { class: "ik-row",
            div { class: "grow",
                div { class: "ik-flex", style: "justify-content:space-between;",
                    span { style: "font-weight:600;", "{acc.username}" }
                    span { class: "ik-pill", "{acc.provider}" }
                }
                div { class: "ik-muted", style: "font-size:13px;",
                    if let Some(u) = &acc.external_username {
                        {
                            i18n.args(
                                "account.sync.connectedAs",
                                &[("user", u), ("when", &last_sync)],
                            )
                        }
                    } else {
                        {i18n.args("console.sync.lastSync", &[("when", &last_sync)])}
                    }
                }
                div { class: "ik-mono ik-muted", style: "font-size:11px;",
                    if acc.auto_sync_enabled {
                        {i18n.t("console.sync.autoOn")}
                    } else {
                        {i18n.t("console.sync.autoOff")}
                    }
                    {i18n.args("console.sync.policy", &[("policy", &acc.conflict_policy)])}
                    if acc.pending_conflicts > 0 {
                        span { style: "color:var(--acc);",
                            {i18n.plural("console.sync.pendingConflicts", acc.pending_conflicts, &[])}
                        }
                    }
                }
                if let Some(err) = &acc.last_error {
                    div { style: "font-size:12px;color:var(--acc);", "{err}" }
                }
            }
            button { class: "ik-btn", disabled: *busy.read(), onclick: pull,
                {i18n.t("console.sync.forcePull")}
            }
            button { class: "ik-btn", disabled: *busy.read(), onclick: push,
                {i18n.t("console.sync.forcePush")}
            }
            button { class: "ik-btn", disabled: *busy.read(), onclick: unlink,
                {i18n.t("console.sync.unlink")}
            }
        }
    }
}
