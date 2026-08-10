//! Sync administration: the linked-account directory and the per-series mapping tools.

mod enrichment;
mod inspector;
mod queues;

use crate::api;
use crate::components::{async_block_list, use_step_up_gate, StepUpGate, StepUpGuard};
use crate::hooks::{use_reload, Reload};
use crate::i18n::use_i18n;
use crate::models::*;
use crate::util::rel_time;
use dioxus::prelude::*;
use enrichment::EnrichmentPanel;
use inkstone_ui::{Button, Pill};
use inspector::SeriesSyncInspector;
use progenitor_client::ResponseValue;
use queues::AssignQueue;
use queues::UnmatchedRemoteQueue;
/// The sync panel's step-up gate, for any component below it.
///
/// Context rather than a prop, unlike the other console panels: the actions that need the
/// elevation sit four components deep across three sub-modules, and threading one handle
/// through every row in between would put it in signatures that otherwise have no interest in
/// it. Provided once by [`SyncAdminPanel`], which is also the only component that renders the
/// prompt.
pub(super) fn use_sync_gate() -> StepUpGate {
    use_context::<StepUpGate>()
}

#[component]
pub(super) fn SyncAdminPanel() -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let reload = use_reload();
    let gate = use_step_up_gate();
    use_context_provider(|| gate);

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

    let no_accounts = i18n.t("console.sync.noAccounts");
    let accounts_body = async_block_list(&accounts, reload, 60, &no_accounts, |rows| {
        let rows = rows.to_vec();
        rsx! {
            for a in rows {
                SyncAccountRow {
                    key: "{a.user_id}-{a.provider}",
                    account: Signal::new(a),
                    reload,
                }
            }
        }
    });

    rsx! {
        StepUpGuard { gate, intro: Some(i18n.t("console.stepUp.intro")) }
        // First, not last: this is the only surface that says whether the catalogue-wide half of
        // the AniList integration is running at all, and every account row below it is about the
        // other half.
        section { style: "margin-bottom:24px;",
            h3 { {i18n.t("console.sync.enrich.title")} }
            p { class: "ik-muted", style: "font-size:13px;margin-top:0;max-width:74ch;",
                {i18n.t("console.sync.enrich.intro")}
            }
            EnrichmentPanel {}
        }
        section { style: "margin-bottom:24px;",
            h3 { {i18n.t("console.sync.accounts")} }
            {accounts_body}
        }
        section { style: "margin-bottom:24px;",
            h3 { {i18n.t("console.sync.inspector")} }
            p { class: "ik-muted", style: "font-size:13px;margin-top:0;",
                {i18n.t("console.sync.inspectorIntro")}
            }
            SeriesSyncInspector { reload }
        }
        section { style: "margin-bottom:24px;",
            h3 { {i18n.t("console.sync.assignQueue")} }
            p { class: "ik-muted", style: "font-size:13px;margin-top:0;",
                {i18n.t("console.sync.assignQueueIntro")}
            }
            AssignQueue { reload }
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
    let gate = use_sync_gate();
    let mut busy = use_signal(|| false);
    let acc = account.read();
    let last_sync = rel_time(i18n, acc.last_synced_at.as_deref());

    let pull = {
        let user_id = UserId(acc.user_id);
        let provider = acc.provider.clone();
        move |_| {
            let provider = provider.clone();
            gate.attempt(move || {
                if *busy.peek() {
                    return;
                }
                busy.set(true);
                let provider = provider.clone();
                spawn(async move {
                    let client = gate.client(api);
                    let body =
                        tankovault_api_client::types::SyncAccountTarget { provider, user_id };
                    match client.admin_sync_pull().body(body).send().await {
                        Ok(_) => reload.bump(),
                        // The row has no error line, so every other failure stays as silent as it
                        // was; an elevation demand has to reach the panel's prompt or the button
                        // does nothing for the rest of the session.
                        Err(e) => {
                            let _refused = gate.refused(api::Refusal::of(&e));
                        }
                    }
                    busy.set(false);
                });
            });
        }
    };

    let push = {
        let user_id = UserId(acc.user_id);
        let provider = acc.provider.clone();
        move |_| {
            let provider = provider.clone();
            gate.attempt(move || {
                if *busy.peek() {
                    return;
                }
                busy.set(true);
                let provider = provider.clone();
                spawn(async move {
                    let client = gate.client(api);
                    let body =
                        tankovault_api_client::types::SyncAccountTarget { provider, user_id };
                    match client.admin_sync_push().body(body).send().await {
                        Ok(_) => reload.bump(),
                        // See `pull` above.
                        Err(e) => {
                            let _refused = gate.refused(api::Refusal::of(&e));
                        }
                    }
                    busy.set(false);
                });
            });
        }
    };

    let unlink = {
        let user_id = UserId(acc.user_id);
        let provider = acc.provider.clone();
        move |_| {
            let provider = provider.clone();
            gate.attempt(move || {
                if *busy.peek() {
                    return;
                }
                busy.set(true);
                let provider = provider.clone();
                spawn(async move {
                    let client = gate.client(api);
                    let body =
                        tankovault_api_client::types::SyncAccountTarget { provider, user_id };
                    match client.admin_sync_unlink().body(body).send().await {
                        Ok(_) => reload.bump(),
                        // See `pull` above.
                        Err(e) => {
                            let _refused = gate.refused(api::Refusal::of(&e));
                        }
                    }
                    busy.set(false);
                });
            });
        }
    };

    rsx! {
        div { class: "ik-row",
            div { class: "grow",
                div { class: "ik-flex", style: "justify-content:space-between;",
                    span { style: "font-weight:600;", "{acc.username}" }
                    Pill {
                        "{acc.provider}"
                    }
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
            Button {
                disabled: *busy.read(),
                on_click: pull,
                {i18n.t("console.sync.forcePull")}
            }
            Button {
                disabled: *busy.read(),
                on_click: push,
                {i18n.t("console.sync.forcePush")}
            }
            Button {
                disabled: *busy.read(),
                on_click: unlink,
                {i18n.t("console.sync.unlink")}
            }
        }
    }
}
