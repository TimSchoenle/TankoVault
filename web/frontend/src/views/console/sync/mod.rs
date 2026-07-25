//! Sync administration: the linked-account directory and the per-series mapping tools.

mod inspector;
mod queues;

use crate::api;
use crate::components::ErrorBox;
use crate::hooks::{use_reload, Reload};
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
                    .map_err(api::friendly_error)
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
            div { class: "ik-empty", "No linked external accounts." }
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
            h3 { "Linked accounts" }
            {accounts_body}
        }
        section { style: "margin-bottom:24px;",
            h3 { "Series sync inspector" }
            p { class: "ik-muted", style: "font-size:13px;margin-top:0;",
                "Open any series to see its info and what it is synced to. Fix a wrong external id or add a missing one by hand."
            }
            SeriesSyncInspector { selected, reload }
        }
        section { style: "margin-bottom:24px;",
            h3 { "Assign queue" }
            p { class: "ik-muted", style: "font-size:13px;margin-top:0;",
                "Series with no mapping for the selected provider yet — the ones auto-matching was not confident about. Assign an id or open the inspector."
            }
            AssignQueue { selected, reload }
        }
        section {
            h3 { "Match every loaded entry" }
            p { class: "ik-muted", style: "font-size:13px;margin-top:0;",
                "Fetched remote entries the auto-matcher could not confidently link. Each one comes with ranked suggestions and a link to open it on the provider; inspect any candidate, then match it — this maps it, imports it onto the user's watchlist, and clears it here."
            }
            UnmatchedRemoteQueue { reload }
        }
    }
}

#[component]
pub(super) fn SyncAccountRow(account: Signal<AdminSyncAccount>, reload: Reload) -> Element {
    let api = api::use_api();
    let mut busy = use_signal(|| false);
    let acc = account.read();
    let last_sync = rel_time(acc.last_synced_at.as_deref());

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
                        "Connected as {u} · last sync {last_sync}"
                    } else {
                        "last sync {last_sync}"
                    }
                }
                div { class: "ik-mono ik-muted", style: "font-size:11px;",
                    if acc.auto_sync_enabled { "auto-sync on" } else { "auto-sync off" }
                    " · policy {acc.conflict_policy}"
                    if acc.pending_conflicts > 0 {
                        span { style: "color:var(--acc);", " · {acc.pending_conflicts} pending conflicts" }
                    }
                }
                if let Some(err) = &acc.last_error {
                    div { style: "font-size:12px;color:var(--acc);", "{err}" }
                }
            }
            button { class: "ik-btn", disabled: *busy.read(), onclick: pull, "Force pull" }
            button { class: "ik-btn", disabled: *busy.read(), onclick: push, "Force push" }
            button { class: "ik-btn", disabled: *busy.read(), onclick: unlink, "Unlink" }
        }
    }
}
