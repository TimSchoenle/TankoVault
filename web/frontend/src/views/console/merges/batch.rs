//! Owns reverting a reviewed list of merges in one go.
//!
//! The lists come from queries an operator runs and reviews outside the console, so the input is
//! pasted decision ids. A dry run reports what would be attempted; the real run is offered only
//! for the exact list a dry run just checked.

use crate::api;
use crate::components::StepUpGate;
use crate::i18n::{use_i18n, Translator};
use crate::state::capabilities::use_capabilities;
use crate::views::console::RefreshTick;
use crate::wire::types::{MergeRevertBatch, MergeRevertBatchOutcome, Permission};
use dioxus::prelude::*;
use inkstone_ui::{Button, Size, Tone};

/// Most decisions the endpoint takes in one request.
const MAX_BATCH: usize = 50;

/// Every UUID in `text`, in order, without duplicates. Anything else (commas, quotes, a pasted
/// query result's other columns) is ignored.
fn parse_ids(text: &str) -> Vec<uuid::Uuid> {
    let mut ids = Vec::new();
    for token in text.split(|c: char| !(c.is_ascii_hexdigit() || c == '-')) {
        if let Ok(id) = uuid::Uuid::parse_str(token) {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
    }
    ids
}

#[component]
pub(super) fn BatchRevert(gate: StepUpGate, tick: RefreshTick) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let caps = use_capabilities();
    let mut open = use_signal(|| false);
    let mut text = use_signal(String::new);
    let mut reason = use_signal(String::new);
    let mut busy = use_signal(|| false);
    let mut notice = use_signal(String::new);
    let mut result = use_signal(|| None::<MergeRevertBatch>);
    // The ids the last dry run checked. A real run is offered only for exactly those.
    let mut checked = use_signal(Vec::<uuid::Uuid>::new);

    let ids = parse_ids(&text.read());
    let too_many = ids.len() > MAX_BATCH;
    let can_apply = !ids.is_empty() && *checked.read() == ids;

    let run = use_callback(move |dry_run: bool| {
        let ids = parse_ids(&text.peek());
        let why = reason.peek().trim().to_owned();
        if ids.is_empty() || ids.len() > MAX_BATCH {
            return;
        }
        if why.is_empty() {
            notice.set(i18n.t("console.merges.batch.reasonRequired"));
            return;
        }
        if *busy.peek() {
            return;
        }
        busy.set(true);
        notice.set(String::new());
        spawn(async move {
            let client = gate.client(api);
            match client
                .revert_merge_decisions()
                .body_map(|body| {
                    body.decision_ids(ids.clone())
                        .reason(why.clone())
                        .dry_run(Some(dry_run))
                })
                .send()
                .await
            {
                Ok(response) => {
                    let batch = response.into_inner();
                    checked.set(if dry_run { ids } else { Vec::new() });
                    if !dry_run {
                        tick.bump();
                    }
                    result.set(Some(batch));
                }
                Err(e) => {
                    if !dry_run {
                        // The batch is not one transaction: a real run that failed part way may
                        // have reverted some decisions, so the dry run no longer describes it.
                        checked.set(Vec::new());
                        result.set(None);
                        tick.bump();
                    }
                    if !gate.refused(api::Refusal::of(&e)) {
                        notice.set(i18n.args(
                            "console.decisions.actionFailed",
                            &[("message", &api::guarded_error(i18n, e))],
                        ));
                    }
                }
            }
            busy.set(false);
        });
    });

    // After every hook, so the hook order is the same whether or not the section renders.
    if !caps.can(Permission::MergeRevert) {
        return rsx! {};
    }

    rsx! {
        div { class: "ik-card", style: "margin:0 0 12px;padding:10px;",
            div { class: "ik-flex", style: "gap:8px;align-items:center;flex-wrap:wrap;",
                Button {
                    size: Size::Sm,
                    on_click: move |_| {
                        let now = *open.peek();
                        open.set(!now);
                    },
                    {i18n.t("console.merges.batch.toggle")}
                }
                span { class: "ik-muted", style: "font-size:12px;max-width:74ch;",
                    {i18n.t("console.merges.batch.intro")}
                }
            }
            if *open.read() {
                label { style: "display:block;margin-top:10px;",
                    span { class: "ik-sec-lbl", {i18n.t("console.merges.batch.idsLabel")} }
                    textarea {
                        class: "ik-input",
                        style: "width:100%;min-height:96px;margin-top:6px;font-family:var(--mono);font-size:12px;",
                        placeholder: i18n.t("console.merges.batch.idsPlaceholder"),
                        value: "{text}",
                        oninput: move |event: FormEvent| text.set(event.value()),
                    }
                }
                p { class: "ik-muted", style: "font-size:12px;margin:4px 0 0;",
                    if too_many {
                        {i18n.args("console.merges.batch.tooMany", &[("max", &MAX_BATCH.to_string())])}
                    } else {
                        {i18n.args("console.merges.batch.parsed", &[("count", &ids.len().to_string())])}
                    }
                }
                label { style: "display:block;margin-top:10px;",
                    span { class: "ik-sec-lbl", {i18n.t("console.merges.reasonLabel")} }
                    input {
                        class: "ik-input",
                        style: "width:100%;margin-top:6px;",
                        r#type: "text",
                        placeholder: i18n.t("console.decisions.reasonPlaceholder"),
                        value: "{reason}",
                        oninput: move |event: FormEvent| reason.set(event.value()),
                    }
                }
                div { class: "ik-flex", style: "gap:8px;margin-top:11px;flex-wrap:wrap;",
                    Button {
                        size: Size::Sm,
                        tone: Tone::Primary,
                        disabled: ids.is_empty() || too_many || *busy.read(),
                        on_click: move |_| gate.attempt(move || run.call(true)),
                        {i18n.t("console.merges.batch.dryRun")}
                    }
                    Button {
                        size: Size::Sm,
                        tone: Tone::Danger,
                        disabled: !can_apply || too_many || *busy.read(),
                        on_click: move |_| gate.attempt(move || run.call(false)),
                        {i18n.args("console.merges.batch.apply", &[("count", &ids.len().to_string())])}
                    }
                }
                if !notice.read().is_empty() {
                    div { class: "ik-note", style: "margin-top:10px;", "{notice}" }
                }
                if let Some(batch) = result.read().clone() {
                    BatchResult { batch }
                }
            }
        }
    }
}

#[component]
fn BatchResult(batch: MergeRevertBatch) -> Element {
    let i18n = use_i18n();
    let summary = i18n.args(
        if batch.dry_run {
            "console.merges.batch.dryRunDone"
        } else {
            "console.merges.batch.done"
        },
        &[
            ("reverted", &batch.reverted.to_string()),
            ("skipped", &batch.skipped.to_string()),
        ],
    );
    rsx! {
        p { style: "font-size:12px;margin:10px 0 6px;", "{summary}" }
        div { class: "ik-tablewrap scroll",
            table { class: "ik-table ik-table-compact",
                thead {
                    tr {
                        th { {i18n.t("console.merges.batch.col.decision")} }
                        th { {i18n.t("console.merges.batch.col.outcome")} }
                    }
                }
                tbody {
                    for item in batch.items.iter() {
                        tr { key: "{item.decision_id}",
                            td { class: "ik-mono", "{item.decision_id}" }
                            td {
                                {outcome_label(i18n, item.outcome, batch.dry_run)}
                                if let Some(detail) = item.detail.clone() {
                                    span { class: "ik-muted", " — {detail}" }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn outcome_label(i18n: Translator, outcome: MergeRevertBatchOutcome, dry_run: bool) -> String {
    i18n.t(match outcome {
        MergeRevertBatchOutcome::Reverted if dry_run => "console.merges.batch.outcome.wouldRevert",
        MergeRevertBatchOutcome::Reverted => "console.merges.batch.outcome.reverted",
        MergeRevertBatchOutcome::NotFound => "console.merges.batch.outcome.notFound",
        MergeRevertBatchOutcome::NothingToRevert => "console.merges.batch.outcome.nothingToRevert",
        MergeRevertBatchOutcome::Refused => "console.merges.batch.outcome.refused",
    })
}

#[cfg(test)]
mod tests {
    use super::parse_ids;

    #[test]
    fn ids_are_found_in_any_pasted_shape_once_each() {
        let a = "0192d3a4-0000-7000-8000-000000000001";
        let b = "0192d3a4-0000-7000-8000-000000000002";
        let pasted = format!("{a}, '{b}'\n{a} not-an-id 42");
        let ids = parse_ids(&pasted);
        assert_eq!(ids.len(), 2);
        assert_eq!(ids[0].to_string(), a);
        assert_eq!(ids[1].to_string(), b);
    }
}
