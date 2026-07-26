//! The GDPR data-subject request queue: what people have asked for, when it is due, and the
//! actions that answer it.
//!
//! Ordered by urgency, with overdue requests marked — an overdue request is a compliance breach
//! in progress, so the queue's job is to make that impossible to scroll past.
//!
//! Fulfilment is deliberately two buttons rather than one. "Resolve" records how a request was
//! answered; "export" and "erase" actually do the thing. Keeping them apart is what lets the
//! trail distinguish *we said we did it* from *we did it*, and it is why the destructive action
//! asks for the subject's username back.

use crate::api;
use crate::components::async_list;
use crate::hooks::{use_busy, use_outcome, use_reload, Busy, Reload};
use crate::i18n::use_i18n;
use crate::models::{RequestKindExt as _, RequestStatusExt as _};
use crate::state::capabilities::use_capabilities;
use crate::util::iso_date;
use crate::views::console::RefreshTick;
use crate::wire::types::{
    AdminRequestRow, FulfilErasure, Permission, RequestKind, RequestStatus, ResolveRequest,
};
use dioxus::prelude::*;
use progenitor_client::ResponseValue;

#[component]
pub(super) fn PrivacyQueuePanel(tick: RefreshTick) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let caps = use_capabilities();
    let reload = use_reload();
    let mut include_resolved = use_signal(|| false);

    let can_write = caps.can(Permission::PrivacyWrite);
    let can_export = caps.can(Permission::PrivacyExport);
    let can_erase = can_write && caps.can(Permission::UsersDelete);

    let requests = use_resource(move || {
        tick.track();
        reload.track();
        let show_all = *include_resolved.read();
        let client = api.client();
        async move {
            client
                .list_privacy_queue()
                .include_resolved(show_all)
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });

    rsx! {
        section { style: "margin-bottom:18px;",
            div { class: "ik-flex", style: "justify-content:space-between;align-items:center;",
                h3 { style: "margin:0;", {i18n.t("console.tab.privacy")} }
                label { class: "ik-flex", style: "gap:6px;font-size:13px;",
                    input {
                        r#type: "checkbox",
                        checked: *include_resolved.read(),
                        onchange: move |e| include_resolved.set(e.checked()),
                    }
                    {i18n.t("console.privacy.includeResolved")}
                }
            }
            p { class: "ik-muted", style: "font-size:13px;max-width:70ch;",
                {i18n.t("console.privacy.intro")}
            }
            {
                async_list(
                    &requests,
                    reload,
                    || rsx! { crate::components::SkeletonRows { count: 3, height: 22 } },
                    &i18n.t("console.privacy.empty"),
                    |rows| rsx! {
                        for row in rows.iter().cloned() {
                            QueueRow {
                                key: "{row.request.id}",
                                row,
                                can_write,
                                can_export,
                                can_erase,
                                reload,
                            }
                        }
                    },
                )
            }
        }
    }
}

/// One queue entry: who asked, for what, by when, and what can be done about it.
#[component]
fn QueueRow(
    row: AdminRequestRow,
    can_write: bool,
    can_export: bool,
    can_erase: bool,
    reload: Reload,
) -> Element {
    let i18n = use_i18n();
    let busy = use_busy();
    let mut confirm_erase = use_signal(|| false);
    let mut typed = use_signal(String::new);
    let outcome = use_outcome();

    let id = row.request.id;
    let open = row.request.status.is_open();
    // Once the subject is gone there is nobody left to serve, which for a completed erasure is
    // the expected end state rather than a fault.
    let subject_present = row.user_id.is_some();
    let subject = row
        .username
        .clone()
        .unwrap_or_else(|| i18n.t("console.privacy.subjectErased"));
    let filed = iso_date(Some(&row.request.requested_at)).to_owned();
    let due = iso_date(Some(&row.request.due_at)).to_owned();

    let matches_subject = row
        .username
        .as_ref()
        .is_some_and(|name| typed.read().trim() == name);

    rsx! {
        div { class: "ik-row", style: "align-items:flex-start;",
            div { class: "grow",
                div { class: "ik-flex", style: "gap:8px;align-items:center;",
                    span {
                        class: if row.overdue { "ik-pill vermilion" } else { "ik-pill" },
                        {i18n.t(row.request.status.label_key())}
                    }
                    strong { style: "font-size:13px;", {i18n.t(row.request.kind.label_key())} }
                    span { class: "ik-muted", style: "font-size:13px;", "{subject}" }
                    if row.overdue {
                        span { class: "ik-pill vermilion", {i18n.t("console.privacy.overdue")} }
                    }
                }
                div { class: "ik-mono ik-muted", style: "font-size:11px;margin-top:2px;",
                    {i18n.args("console.privacy.filedOn", &[("date", &filed)])}
                    if open {
                        " · "
                        {i18n.args("console.privacy.dueBy", &[("date", &due)])}
                    }
                    if let Some(email) = row.email.clone() {
                        " · "
                        "{email}"
                    }
                }
                if let Some(detail) = row.request.detail.clone() {
                    p { style: "font-size:12px;margin:6px 0 0;max-width:74ch;", "“{detail}”" }
                }
                if let Some(note) = row.request.resolution_note.clone() {
                    p { class: "ik-muted", style: "font-size:12px;margin:4px 0 0;", "{note}" }
                }
                if let Some(by) = row.claimed_by.clone() {
                    div { class: "ik-mono ik-muted", style: "font-size:11px;margin-top:2px;",
                        {i18n.args("console.privacy.claimedBy", &[("user", &by)])}
                    }
                }
                crate::components::OutcomeLine { outcome: outcome.read().clone() }

                if *confirm_erase.read() {
                    div { class: "ik-field", style: "margin-top:10px;max-width:420px;",
                        label { r#for: "tv-erase-{id}",
                            {i18n.args("console.privacy.eraseConfirmLabel", &[("username", &subject)])}
                        }
                        input {
                            id: "tv-erase-{id}",
                            class: "ik-input",
                            autocomplete: "off",
                            value: "{typed}",
                            oninput: move |e| typed.set(e.value()),
                        }
                    }
                    div { class: "ik-flex", style: "margin-top:8px;",
                        EraseButton {
                            id,
                            confirm: typed.read().trim().to_owned(),
                            enabled: matches_subject,
                            busy,
                            reload,
                            outcome,
                        }
                        button {
                            class: "ik-btn",
                            onclick: move |_| {
                                confirm_erase.set(false);
                                typed.set(String::new());
                            },
                            {i18n.t("common.cancel")}
                        }
                    }
                }
            }

            div { class: "ik-flex", style: "gap:6px;flex-shrink:0;flex-wrap:wrap;",
                if can_write && row.request.status == RequestStatus::Pending {
                    ActionButton {
                        id,
                        action: QueueAction::Claim,
                        label: i18n.t("console.privacy.claim"),
                        busy,
                        reload,
                        outcome,
                    }
                }
                if can_export && row.request.kind.needs_export() && subject_present {
                    ExportButton { id, busy, outcome }
                }
                if can_erase && open && row.request.kind == RequestKind::Erasure && subject_present
                    && !*confirm_erase.read()
                {
                    button {
                        class: "ik-btn",
                        style: "color:var(--vermilion);",
                        onclick: move |_| confirm_erase.set(true),
                        {i18n.t("console.privacy.erase")}
                    }
                }
                if can_write && open {
                    ActionButton {
                        id,
                        action: QueueAction::Complete,
                        label: i18n.t("console.privacy.complete"),
                        busy,
                        reload,
                        outcome,
                    }
                    ActionButton {
                        id,
                        action: QueueAction::Reject,
                        label: i18n.t("console.privacy.reject"),
                        busy,
                        reload,
                        outcome,
                    }
                }
            }
        }
    }
}

/// The non-destructive queue transitions.
#[derive(Clone, Copy, PartialEq, Eq)]
enum QueueAction {
    Claim,
    Complete,
    Reject,
}

/// Claim, complete or reject a request.
#[component]
fn ActionButton(
    id: uuid::Uuid,
    action: QueueAction,
    label: String,
    busy: Busy,
    reload: Reload,
    outcome: Signal<crate::hooks::Outcome>,
) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let mut outcome = outcome;

    let click = move |_| {
        if !busy.claim() {
            return;
        }
        outcome.set(None);
        let client = api.client();
        spawn(async move {
            let result = match action {
                QueueAction::Claim => client
                    .claim_privacy_request()
                    .id(id)
                    .send()
                    .await
                    .map(|_| ()),
                QueueAction::Complete => client
                    .resolve_privacy_request()
                    .id(id)
                    .body(ResolveRequest {
                        status: RequestStatus::Completed,
                        note: None,
                    })
                    .send()
                    .await
                    .map(|_| ()),
                // A rejection must state its reasons (Art. 12(4)) and the server enforces that,
                // so this sends a standing one rather than offering a button that always fails.
                QueueAction::Reject => client
                    .resolve_privacy_request()
                    .id(id)
                    .body(ResolveRequest {
                        status: RequestStatus::Rejected,
                        note: Some(i18n.t("console.privacy.defaultRejectionReason")),
                    })
                    .send()
                    .await
                    .map(|_| ()),
            };
            match result {
                Ok(()) => reload.bump(),
                Err(e) => outcome.set(Some(Err(api::friendly_error(i18n, e)))),
            }
            busy.release();
        });
    };

    rsx! {
        button { class: "ik-btn", disabled: busy.is_busy(), onclick: click, "{label}" }
    }
}

/// Download the subject's export to fulfil an access or portability request.
#[component]
fn ExportButton(id: uuid::Uuid, busy: Busy, outcome: Signal<crate::hooks::Outcome>) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let mut outcome = outcome;

    let click = move |_| {
        if !busy.claim() {
            return;
        }
        outcome.set(None);
        let client = api.client();
        spawn(async move {
            // The filename carries the *request* id, not the subject's: the operator is filing
            // this against a request, and an export named after a person is one careless
            // forward away from being personal data in someone's downloads folder.
            let filename = format!("tankovault-export-{id}.json");
            match client.export_subject_data().id(id).send().await {
                Ok(response) => {
                    let body = response.into_inner();
                    let saved = serde_json::to_string_pretty(&body)
                        .map_err(|_| i18n.t("console.privacy.exportFailed"))
                        .and_then(|json| {
                            crate::util::save_text_file(&filename, "application/json", &json)
                        });
                    match saved {
                        Ok(()) => outcome.set(Some(Ok(i18n.t("console.privacy.exportDone")))),
                        Err(message) => outcome.set(Some(Err(message))),
                    }
                }
                Err(e) => outcome.set(Some(Err(api::friendly_error(i18n, e)))),
            }
            busy.release();
        });
    };

    rsx! {
        button { class: "ik-btn", disabled: busy.is_busy(), onclick: click,
            {i18n.t("console.privacy.export")}
        }
    }
}

/// Carry out an erasure request. Disabled until the subject's username has been typed back.
#[component]
fn EraseButton(
    id: uuid::Uuid,
    confirm: String,
    enabled: bool,
    busy: Busy,
    reload: Reload,
    outcome: Signal<crate::hooks::Outcome>,
) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let mut outcome = outcome;

    let click = move |_| {
        if !busy.claim() {
            return;
        }
        outcome.set(None);
        let body = FulfilErasure {
            confirm_username: confirm.clone(),
        };
        let client = api.client();
        spawn(async move {
            match client.fulfil_erasure().id(id).body(body).send().await {
                Ok(_) => reload.bump(),
                Err(e) => outcome.set(Some(Err(api::friendly_error(i18n, e)))),
            }
            busy.release();
        });
    };

    rsx! {
        button {
            class: "ik-btn",
            style: "color:var(--vermilion);",
            disabled: busy.is_busy() || !enabled,
            onclick: click,
            {i18n.t("console.privacy.eraseConfirmCta")}
        }
    }
}
