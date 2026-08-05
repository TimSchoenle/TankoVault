//! The GDPR data-subject request queue: what people have asked for, when it is due, and the
//! actions that answer it. Ordered by urgency, with overdue requests marked, since an overdue
//! request is a compliance breach in progress.
//!
//! Fulfilment is deliberately two buttons rather than one: "resolve" records how a request was
//! answered, while "export" and "erase" do the thing. Merging them would lose the trail's ability
//! to distinguish *we said we did it* from *we did it* — also why erasure requires the subject's
//! username typed back.

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

/// What this reader is allowed to do to the queue at all.
///
/// `erase` is deliberately the conjunction of two permissions: answering an erasure request
/// destroys an account, so holding the privacy queue's write permission is not on its own
/// enough — the reader must also be trusted to delete users.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct QueuePermits {
    write: bool,
    export: bool,
    erase: bool,
}

impl QueuePermits {
    fn of(caps: &crate::state::capabilities::CapabilitySet) -> Self {
        let write = caps.can(Permission::PrivacyWrite);
        Self {
            write,
            export: caps.can(Permission::PrivacyExport),
            erase: write && caps.can(Permission::UsersDelete),
        }
    }
}

/// A control a queue row can offer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum RowAction {
    /// Take ownership of a request nobody is working.
    Claim,
    /// Release the subject's data export.
    Export,
    /// Carry out the erasure.
    Erase,
    /// Record how the request was answered.
    Resolve,
}

/// Whether a row offers `action`, given the reader's permissions and the request's own state.
///
/// Every arm that acts on the person requires the person to still be there: a completed erasure
/// leaves its row in the queue with `user_id: None`, and offering export or erasure over an
/// account that is already gone is a call the server refuses.
///
/// `needs_export` is the server's answer rather than a re-derivation from `kind` — the set of
/// kinds that disclose an export belongs where the export is produced (see the field's own doc).
pub(super) fn offers(row: &AdminRequestRow, permits: QueuePermits, action: RowAction) -> bool {
    let open = row.request.status.is_open();
    let subject_present = row.user_id.is_some();
    match action {
        RowAction::Claim => permits.write && row.request.status == RequestStatus::Pending,
        RowAction::Export => permits.export && row.needs_export && subject_present,
        RowAction::Erase => {
            permits.erase && open && row.request.kind == RequestKind::Erasure && subject_present
        }
        RowAction::Resolve => permits.write && open,
    }
}

/// Whether the typed confirmation releases the erasure.
///
/// A subject with no username left can never be confirmed: matching "nothing typed" against
/// "no name" would arm the most destructive control on the page with an empty field.
pub(super) fn erasure_confirmed(username: Option<&str>, typed: &str) -> bool {
    username.is_some_and(|name| typed.trim() == name)
}

#[component]
pub(super) fn PrivacyQueuePanel(tick: RefreshTick) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let caps = use_capabilities();
    let reload = use_reload();
    let mut include_resolved = use_signal(|| false);

    let permits = QueuePermits::of(&caps);

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
                            QueueRow { key: "{row.request.id}", row, permits, reload }
                        }
                    },
                )
            }
        }
    }
}

/// One queue entry: who asked, for what, by when, and what can be done about it.
#[component]
fn QueueRow(row: AdminRequestRow, permits: QueuePermits, reload: Reload) -> Element {
    let i18n = use_i18n();
    let busy = use_busy();
    let mut confirm_erase = use_signal(|| false);
    let mut typed = use_signal(String::new);
    let outcome = use_outcome();

    let id = row.request.id;
    let open = row.request.status.is_open();
    let can_erase = offers(&row, permits, RowAction::Erase);
    let subject = row
        .username
        .clone()
        .unwrap_or_else(|| i18n.t("console.privacy.subjectErased"));
    let filed = iso_date(Some(&row.request.requested_at)).to_owned();
    let due = iso_date(Some(&row.request.due_at)).to_owned();

    let matches_subject = erasure_confirmed(row.username.as_deref(), &typed.read());

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
                if offers(&row, permits, RowAction::Claim) {
                    ActionButton {
                        id,
                        action: QueueAction::Claim,
                        label: i18n.t("console.privacy.claim"),
                        busy,
                        reload,
                        outcome,
                    }
                }
                if offers(&row, permits, RowAction::Export) {
                    ExportButton { id, busy, outcome }
                }
                if can_erase && !*confirm_erase.read() {
                    button {
                        class: "ik-btn",
                        style: "color:var(--vermilion);",
                        onclick: move |_| confirm_erase.set(true),
                        {i18n.t("console.privacy.erase")}
                    }
                }
                if offers(&row, permits, RowAction::Resolve) {
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
                // Rejections must state reasons (Art. 12(4)); the server enforces that, so this sends a standing one.
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
            // Filename carries the request id, not the subject's: naming it after a person risks
            // the export becoming personal data in someone's downloads folder.
            let filename = format!("tankovault-export-{id}.json");
            match client.export_subject_data().id(id).send().await {
                Ok(response) => {
                    let body = response.into_inner();
                    let saved = serde_json::to_string_pretty(&body)
                        .map_err(|_| "console.privacy.exportFailed")
                        .and_then(|json| {
                            crate::util::save_text_file(&filename, "application/json", &json)
                        });
                    match saved {
                        Ok(()) => outcome.set(Some(Ok(i18n.t("console.privacy.exportDone")))),
                        Err(key) => outcome.set(Some(Err(i18n.t(key)))),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::types::RequestRow;

    const ALL: QueuePermits = QueuePermits {
        write: true,
        export: true,
        erase: true,
    };

    fn row(kind: RequestKind, status: RequestStatus, subject: Option<&str>) -> AdminRequestRow {
        AdminRequestRow {
            claimed_by: None,
            email: None,
            // The server's answer; the console must not re-derive it from `kind`.
            needs_export: kind != RequestKind::Erasure,
            overdue: false,
            request: RequestRow {
                detail: None,
                due_at: "2026-08-28T00:00:00Z".to_owned(),
                id: uuid::Uuid::from_u128(1),
                kind,
                requested_at: "2026-07-29T00:00:00Z".to_owned(),
                resolution_note: None,
                resolved_at: None,
                status,
            },
            resolved_by: None,
            user_id: subject.map(|_| "11111111-1111-1111-1111-111111111111".to_owned()),
            username: subject.map(ToOwned::to_owned),
        }
    }

    /// Erasure is gated on two permissions, not one. `privacy.write` alone advances the queue's
    /// paperwork; destroying the account behind it also needs `users.delete`, and a reader with
    /// only the former must not be shown a control the server would refuse.
    #[test]
    fn erasing_needs_both_the_privacy_and_the_delete_permission() {
        let request = row(RequestKind::Erasure, RequestStatus::Pending, Some("kaori"));
        let write_only = QueuePermits {
            erase: false,
            ..ALL
        };
        assert!(!offers(&request, write_only, RowAction::Erase));
        assert!(offers(&request, ALL, RowAction::Erase));
    }

    /// A completed erasure leaves its row in the queue with the subject gone. Every action that
    /// acts on the person must disappear with them, or the row offers calls the server refuses.
    #[test]
    fn an_already_erased_subject_offers_nothing_that_acts_on_them() {
        let gone = row(RequestKind::Erasure, RequestStatus::InProgress, None);
        assert!(!offers(&gone, ALL, RowAction::Erase));
        assert!(!offers(&gone, ALL, RowAction::Export));
        // Resolving is paperwork about the request, not about the person, so it stays.
        assert!(offers(&gone, ALL, RowAction::Resolve));
    }

    /// Erasure is offered on erasure requests only — never as a shortcut out of an access or
    /// rectification request that merely happens to be open.
    #[test]
    fn only_an_erasure_request_offers_erasure() {
        for kind in [
            RequestKind::Access,
            RequestKind::Portability,
            RequestKind::Rectification,
        ] {
            let request = row(kind, RequestStatus::Pending, Some("kaori"));
            assert!(
                !offers(&request, ALL, RowAction::Erase),
                "`{kind}` offered the erase control"
            );
        }
    }

    /// A closed request is finished: nothing may still be done to it, and claiming is offered
    /// only while it is unclaimed.
    #[test]
    fn a_resolved_request_offers_no_transitions() {
        for status in [RequestStatus::Completed, RequestStatus::Rejected] {
            let request = row(RequestKind::Erasure, status, Some("kaori"));
            assert!(
                !offers(&request, ALL, RowAction::Erase),
                "`{status}` offered the erase control"
            );
            assert!(
                !offers(&request, ALL, RowAction::Resolve),
                "`{status}` offered a resolution"
            );
            assert!(
                !offers(&request, ALL, RowAction::Claim),
                "`{status}` offered a claim"
            );
        }
        let taken = row(RequestKind::Access, RequestStatus::InProgress, Some("kaori"));
        assert!(!offers(&taken, ALL, RowAction::Claim));
    }

    /// The export control follows the server's `needs_export`, which is why that field exists —
    /// the console re-deriving it from `kind` was FRONTEND F10.
    #[test]
    fn the_export_control_follows_the_servers_own_answer() {
        let mut request = row(RequestKind::Access, RequestStatus::Pending, Some("kaori"));
        assert!(offers(&request, ALL, RowAction::Export));
        request.needs_export = false;
        assert!(!offers(&request, ALL, RowAction::Export));
    }

    /// The typed confirmation must match the subject exactly, and an already-erased subject can
    /// never be confirmed — comparing an empty field against a missing name would arm the most
    /// destructive control on the page with nothing typed at all.
    #[test]
    fn the_erasure_confirmation_needs_the_exact_username() {
        assert!(erasure_confirmed(Some("kaori"), "  kaori "));
        assert!(!erasure_confirmed(Some("kaori"), "Kaori"));
        assert!(!erasure_confirmed(Some("kaori"), ""));
        assert!(!erasure_confirmed(None, ""));
        assert!(!erasure_confirmed(None, "   "));
    }
}
