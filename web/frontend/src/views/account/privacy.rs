//! Privacy & data — the reader's own GDPR controls: download everything, file a formal
//! request, and delete the account.
//!
//! Each section shows only when its feature is on. When self-service deletion is off, the
//! erasure *right* isn't gone — it moves into the request queue, which is why requests list
//! erasure as a kind an operator can act on.

use crate::api;
use crate::components::{
    async_list, use_step_up_gate, OutcomeLine, PanelCard, SkeletonRows, StepUpGate, StepUpGuard,
};
use crate::hooks::{use_busy, use_outcome, use_reload, Reload};
use crate::i18n::use_i18n;
use crate::icons::Icon;
use crate::models::{RequestKindExt as _, RequestStatusExt as _};
use crate::state::capabilities::use_capabilities;
use crate::state::use_session;
use crate::util::iso_date;
use crate::wire::types::{DeleteAccount, Feature, NewPrivacyRequest, RequestKind, RequestRow};
use dioxus::prelude::*;
use progenitor_client::ResponseValue;

#[component]
pub(crate) fn PrivacyPanel() -> Element {
    let caps = use_capabilities();
    rsx! {
        if caps.has_feature(Feature::PrivacySelfExport) {
            ExportCard {}
        }
        if caps.has_feature(Feature::PrivacyRequests) {
            RequestsCard {}
        }
        if caps.has_feature(Feature::PrivacySelfErasure) {
            DeleteAccountCard {}
        }
    }
}

/// Download everything the system holds about the reader (GDPR Art. 20).
///
/// Fetched and saved from memory rather than linked to directly: the endpoint is
/// bearer-authenticated, and a plain anchor navigation carries no `Authorization` header. See
/// [`crate::platform::save_text_file`], which also explains why the web build revokes the object
/// URL straight away for a document of this sensitivity.
#[component]
fn ExportCard() -> Element {
    let i18n = use_i18n();
    let api = api::use_api();
    let gate = use_step_up_gate();
    let busy = use_busy();
    let mut outcome = use_outcome();

    let download = move |_| {
        gate.attempt(move || {
            if !busy.claim() {
                return;
            }
            outcome.set(None);
            let client = gate.client(api);
            spawn(async move {
                match client.export_data().send().await {
                    Ok(response) => {
                        let body = response.into_inner();
                        match serde_json::to_string_pretty(&body) {
                            Ok(json) => {
                                match crate::platform::save_text_file(
                                    "tankovault-export.json",
                                    "application/json",
                                    &json,
                                )
                                .await
                                {
                                    Ok(()) => {
                                        outcome
                                            .set(Some(Ok(i18n.t("account.privacy.export.done"))));
                                    }
                                    Err(key) => outcome.set(Some(Err(i18n.t(key)))),
                                }
                            }
                            Err(_) => {
                                outcome.set(Some(Err(i18n.t("account.privacy.export.failed"))));
                            }
                        }
                    }
                    // A `403` here is "confirm it is you", not "you may not". The export is the
                    // single highest-value thing a stolen session can ask for, so the API demands an
                    // elevation — and reporting the raw problem told the owner they lacked
                    // permission to download their own record.
                    Err(e) => {
                        if !gate.refused(api::Refusal::from_status(api::error_status(&e))) {
                            outcome.set(Some(Err(api::friendly_error(i18n, e))));
                        }
                    }
                }
                busy.release();
            });
        });
    };

    rsx! {
        PanelCard { icon: Icon::Download, title: i18n.t("account.privacy.export.title"),
            p { class: "ik-muted", style: "font-size:13px;margin-top:0;",
                {i18n.t("account.privacy.export.intro")}
            }
            OutcomeLine { outcome: outcome.read().clone() }
            StepUpGuard { gate }
            button {
                class: "ik-btn primary",
                style: "margin-top:12px;",
                disabled: busy.is_busy(),
                onclick: download,
                if busy.is_busy() {
                    {i18n.t("account.privacy.export.preparing")}
                } else {
                    {i18n.t("account.privacy.export.cta")}
                }
            }
        }
    }
}

/// Resolve a `<select>` value back to a kind, defaulting to the first option for anything
/// unrecognised — which can only happen if the markup and this function disagree.
fn parse_kind(token: &str) -> RequestKind {
    RequestKind::all()
        .iter()
        .copied()
        .find(|k| k.token() == token)
        .unwrap_or(RequestKind::Access)
}

/// File a data-subject request and watch its progress.
#[component]
fn RequestsCard() -> Element {
    let i18n = use_i18n();
    let api = api::use_api();
    let session = use_session();
    let gate = use_step_up_gate();
    let reload = use_reload();
    let busy = use_busy();
    let mut outcome = use_outcome();
    let mut kind = use_signal(|| RequestKind::Access);
    let mut detail = use_signal(String::new);

    let requests = use_resource(move || {
        reload.track();
        let client = api.client();
        let authed = session.is_authenticated();
        async move {
            if !authed {
                return Ok(Vec::new());
            }
            client
                .list_privacy_requests()
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });

    let submit = move |_| {
        gate.attempt(move || {
            if !busy.claim() {
                return;
            }
            outcome.set(None);
            let body = NewPrivacyRequest {
                kind: *kind.peek(),
                detail: {
                    let text = detail.peek().trim().to_owned();
                    (!text.is_empty()).then_some(text)
                },
            };
            // Elevated: an access request ends with an operator mailing the caller's whole record,
            // and an erasure request ends with the account gone. Both are the export and the
            // deletion above, taking a slower route.
            let client = gate.client(api);
            spawn(async move {
                match client.create_privacy_request().body(body).send().await {
                    Ok(_) => {
                        detail.set(String::new());
                        outcome.set(Some(Ok(i18n.t("account.privacy.requests.filed"))));
                        reload.bump();
                    }
                    Err(e) => {
                        if !gate.refused(api::Refusal::of(&e)) {
                            outcome.set(Some(Err(api::friendly_error(i18n, e))));
                        }
                    }
                }
                busy.release();
            });
        });
    };

    let current_kind = *kind.read();
    rsx! {
        PanelCard { icon: Icon::ShieldLock, title: i18n.t("account.privacy.requests.title"),
            p { class: "ik-muted", style: "font-size:13px;margin-top:0;",
                {i18n.t("account.privacy.requests.intro")}
            }

            div { class: "ik-field",
                label { r#for: "tv-privacy-kind", {i18n.t("account.privacy.requests.kind")} }
                select {
                    id: "tv-privacy-kind",
                    class: "ik-input",
                    value: current_kind.token(),
                    onchange: move |e| kind.set(parse_kind(&e.value())),
                    for k in RequestKind::all().iter().copied() {
                        option { key: "{k.token()}", value: k.token(), {i18n.t(k.label_key())} }
                    }
                }
            }
            div { class: "ik-field",
                label { r#for: "tv-privacy-detail", {i18n.t("account.privacy.requests.detail")} }
                textarea {
                    id: "tv-privacy-detail",
                    class: "ik-input",
                    rows: 3,
                    placeholder: i18n.t("account.privacy.requests.detailPlaceholder"),
                    value: "{detail}",
                    oninput: move |e| detail.set(e.value()),
                }
            }
            OutcomeLine { outcome: outcome.read().clone() }
            // One prompt for the card, shared with the withdraw button on every row below: the
            // grant a reader earns for one of these covers the others for the next few minutes,
            // so a second copy of the question would only be a second place to ask it.
            StepUpGuard { gate }
            button {
                class: "ik-btn primary",
                style: "margin-top:12px;",
                disabled: busy.is_busy(),
                onclick: submit,
                if busy.is_busy() {
                    {i18n.t("common.saving")}
                } else {
                    {i18n.t("account.privacy.requests.submit")}
                }
            }

            div { class: "ik-subhead", style: "margin-top:18px;",
                {i18n.t("account.privacy.requests.mine")}
            }
            {
                async_list(
                    &requests,
                    reload,
                    || rsx! { SkeletonRows { count: 2 } },
                    &i18n.t("account.privacy.requests.empty"),
                    |rows| rsx! {
                        for row in rows.iter().cloned() {
                            RequestRowView { key: "{row.id}", request: row, reload, gate }
                        }
                    },
                )
            }
        }
    }
}

/// One of the reader's own requests, with a withdraw action while it is still open.
///
/// `gate` belongs to the card rather than the row: withdrawing needs an elevation, and a refusal
/// has to open the one prompt the card renders instead of a prompt per row.
#[component]
fn RequestRowView(request: RequestRow, reload: Reload, gate: StepUpGate) -> Element {
    let i18n = use_i18n();
    let api = api::use_api();
    let busy = use_busy();

    let id = request.id;
    let cancel = move |_| {
        gate.attempt(move || {
            if !busy.claim() {
                return;
            }
            // Elevated: withdrawing is how an attacker would silence the rectification request their
            // victim filed about the change they made.
            let client = gate.client(api);
            spawn(async move {
                match client.cancel_privacy_request().id(id).send().await {
                    Ok(_) => reload.bump(),
                    // The row has no error line of its own. A `403` opens the card's prompt, which
                    // is the only outcome a reader can act on; anything else leaves the row as it
                    // was, as it did before the gate existed.
                    Err(e) => {
                        let _refused = gate.refused(api::Refusal::of(&e));
                    }
                }
                busy.release();
            });
        });
    };

    let open = request.status.is_open();
    let filed = iso_date(Some(&request.requested_at)).to_owned();
    let due = iso_date(Some(&request.due_at)).to_owned();

    rsx! {
        div { class: "ik-row",
            div { class: "grow",
                div { style: "font-weight:600;font-size:13px;",
                    {i18n.t(request.status.label_key())}
                    " · "
                    {i18n.t(request.kind.label_key())}
                }
                div { class: "ik-mono ik-muted", style: "font-size:11px;",
                    {i18n.args("account.privacy.requests.filedOn", &[("date", &filed)])}
                    // The deadline is only meaningful while the clock is still running.
                    if open {
                        " · "
                        {i18n.args("account.privacy.requests.dueBy", &[("date", &due)])}
                    }
                }
                if let Some(note) = request.resolution_note.clone() {
                    div { class: "ik-muted", style: "font-size:12px;margin-top:4px;", "{note}" }
                }
            }
            if open {
                button { class: "ik-btn", disabled: busy.is_busy(), onclick: cancel,
                    {i18n.t("account.privacy.requests.withdraw")}
                }
            }
        }
    }
}

/// Delete the account and everything in it (GDPR Art. 17).
///
/// Two barriers, both deliberate: the destructive controls stay hidden until the reader asks
/// for them, and the confirm button stays disabled until they type their own username. The
/// server requires the username too — this is not the check, it is the pause before it.
#[component]
fn DeleteAccountCard() -> Element {
    let i18n = use_i18n();
    let api = api::use_api();
    let session = use_session();
    let caps = use_capabilities();
    let gate = use_step_up_gate();
    let busy = use_busy();
    let mut outcome = use_outcome();
    let mut armed = use_signal(|| false);
    let mut typed = use_signal(String::new);

    let username = session.username().unwrap_or_default();
    let matches_username = !username.is_empty() && typed.read().trim() == username;

    let delete = move |_| {
        gate.attempt(move || {
            if !busy.claim() {
                return;
            }
            outcome.set(None);
            let body = DeleteAccount {
                confirm_username: typed.peek().trim().to_owned(),
            };
            // Elevated in addition to the typed username: the confirmation guards against a
            // misclick, it is not a credential, and on its own it left the single irreversible
            // action on the account reachable by anyone holding a token.
            let client = gate.client(api);
            spawn(async move {
                match client.delete_account().body(body).send().await {
                    Ok(_) => {
                        // The account is gone; clear locally now rather than leaving a shell whose
                        // every request 401s.
                        session.clear();
                        caps.clear();
                    }
                    Err(e) => {
                        if !gate.refused(api::Refusal::from_status(api::error_status(&e))) {
                            outcome.set(Some(Err(api::friendly_error(i18n, e))));
                        }
                        busy.release();
                    }
                }
            });
        });
    };

    rsx! {
        PanelCard { icon: Icon::Delete, title: i18n.t("account.privacy.delete.title"),
            p { class: "ik-muted", style: "font-size:13px;margin-top:0;",
                {i18n.t("account.privacy.delete.intro")}
            }
            StepUpGuard { gate }
            if *armed.read() {
                div { class: "ik-field", style: "margin-top:12px;",
                    label { r#for: "tv-delete-confirm",
                        {i18n.args("account.privacy.delete.confirmLabel", &[("username", &username)])}
                    }
                    input {
                        id: "tv-delete-confirm",
                        class: "ik-input",
                        autocomplete: "off",
                        value: "{typed}",
                        oninput: move |e| typed.set(e.value()),
                    }
                }
                OutcomeLine { outcome: outcome.read().clone() }
                div { class: "ik-flex", style: "margin-top:12px;",
                    button {
                        class: "ik-btn",
                        style: "color:var(--vermilion);",
                        disabled: busy.is_busy() || !matches_username,
                        onclick: delete,
                        {i18n.t("account.privacy.delete.confirmCta")}
                    }
                    button {
                        class: "ik-btn",
                        onclick: move |_| {
                            armed.set(false);
                            typed.set(String::new());
                        },
                        {i18n.t("common.cancel")}
                    }
                }
            } else {
                button {
                    class: "ik-btn",
                    style: "margin-top:12px;color:var(--vermilion);",
                    onclick: move |_| armed.set(true),
                    {i18n.t("account.privacy.delete.cta")}
                }
            }
        }
    }
}
