//! The decision journals: what the automatic merge and the automatic sync did, why, and the two
//! answers to "that was wrong" — undo it, and say so durably.
//!
//! Both halves render the same shape because the operator's question is the same one: a headline
//! naming what happened, the rule that decided it, and an expander holding the itemised evidence.
//! The evidence is the point — a score and a bag of signal names say *that* two things matched,
//! and only the terms say which title matched and what each rule contributed.

use crate::api;
use crate::components::{
    async_view, use_step_up_gate, ListSearch, SkeletonRows, StepUpGate, StepUpGuard,
};
use crate::i18n::{use_i18n, Translator};
use crate::models::*;
use crate::state::capabilities::use_capabilities;
use crate::util::rel_time;
use crate::views::console::{signal_label, use_console_nav, RefreshTick};
use crate::wire::types::Permission;
use dioxus::prelude::*;
use inkstone_ui::{Button, Pill, Size, ToggleButton, Tone};
use progenitor_client::ResponseValue;
/// Rows per page. The server clamps regardless; this is the number that fits a screen without
/// paging becoming the primary interaction.
const PAGE_SIZE: u32 = 50;

/// Rows per page while a search is running.
///
/// The endpoint has no text predicate — a decision is matched on titles, an account name and a
/// provider slug, none of which it indexes — so the search runs over what is loaded. It is
/// therefore worth loading more of the journal while one is being typed: this is the server's own
/// ceiling, so nothing here is asking for a page it will not answer.
const SEARCH_PAGE_SIZE: u32 = 200;

/// Whether `haystack` contains the already-lowercased `needle`.
fn matches(needle: &str, haystack: &[Option<&str>]) -> bool {
    haystack
        .iter()
        .flatten()
        .any(|field| field.to_lowercase().contains(needle))
}

/// Which journal the panel is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Journal {
    Merges,
    Sync,
}

/// The whole surface: a journal switch, then whichever journal is selected.
///
/// The two are separate fetches behind one switch rather than one merged feed, because they are
/// not comparable: a merge decision is about the catalogue and a sync decision is about one
/// reader's shelf, and interleaving them by timestamp produces a list that answers neither
/// question.
#[component]
pub(super) fn DecisionsPanel(tick: RefreshTick) -> Element {
    let i18n = use_i18n();
    let caps = use_capabilities();
    let can_merge = caps.can(Permission::MergeAudit);
    let can_sync = caps.can(Permission::SyncAudit);

    // Open on whichever journal the reader may actually see. Defaulting to merges and rendering
    // a permission message would read as an error to someone holding only `sync.audit`.
    let mut journal = use_signal(|| {
        if can_merge {
            Journal::Merges
        } else {
            Journal::Sync
        }
    });
    let current = *journal.read();

    rsx! {
        section { style: "margin-bottom:18px;",
            h3 { {i18n.t("console.decisions.title")} }
            p { class: "ik-muted", style: "margin:0 0 12px;max-width:78ch;",
                {i18n.t("console.decisions.intro")}
            }
            div { class: "ik-flex", style: "gap:8px;flex-wrap:wrap;margin-bottom:12px;",
                if can_merge {
                    ToggleButton {
                        on: current == Journal::Merges,
                        size: Size::Sm,
                        on_toggle: move |_| journal.set(Journal::Merges),
                        {i18n.t("console.decisions.tab.merges")}
                    }
                }
                if can_sync {
                    ToggleButton {
                        on: current == Journal::Sync,
                        size: Size::Sm,
                        on_toggle: move |_| journal.set(Journal::Sync),
                        {i18n.t("console.decisions.tab.sync")}
                    }
                }
            }
            match current {
                Journal::Merges if can_merge => rsx! { MergeJournal { tick } },
                Journal::Sync if can_sync => rsx! { SyncJournal { tick } },
                _ => rsx! {
                    div { class: "ik-empty", style: "padding:24px;",
                        {i18n.t("console.operatorsOnly")}
                    }
                },
            }
        }
    }
}

/// The automatic-merge journal, newest first.
#[component]
fn MergeJournal(tick: RefreshTick) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let caps = use_capabilities();
    let nav = use_console_nav();
    let can_revert = caps.can(Permission::MergeRevert);
    let mut outcome = use_signal(String::new);
    let mut blocked_only = use_signal(|| false);
    let notice = use_signal(String::new);
    // One gate for the journal, shared with every row: the rows report through the same
    // `notice`, so the prompt belongs beside it rather than duplicated per decision.
    let gate = use_step_up_gate();

    let filter_outcome = outcome.read().clone();
    let only_blocked = *blocked_only.read();
    // In the URL like every other console filter, so an operator can send "the journal, around
    // this title" rather than describing where to scroll to.
    let search = nav.query().q;
    let searching = !search.trim().is_empty();
    let rows = use_resource(use_reactive!(|(
        filter_outcome,
        only_blocked,
        searching,
    )| {
        tick.track();
        let client = api.client();
        async move {
            let depth = if searching {
                SEARCH_PAGE_SIZE
            } else {
                PAGE_SIZE
            };
            let mut request = client.list_merge_decisions().limit(depth);
            if !filter_outcome.is_empty() {
                request = request.outcome(filter_outcome);
            }
            if only_blocked {
                request = request.blocked(true);
            }
            request
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    }));

    let needle = search.trim().to_lowercase();
    let keep = move |decision: &MergeDecision| {
        needle.is_empty()
            || matches(
                &needle,
                &[
                    Some(decision.left_title.as_str()),
                    Some(decision.right_title.as_str()),
                    Some(decision.outcome.as_str()),
                    Some(decision.reason.as_str()),
                ],
            )
    };

    rsx! {
        div { class: "ik-flex", style: "gap:8px;flex-wrap:wrap;margin-bottom:10px;",
            select {
                class: "ik-input",
                "aria-label": i18n.t("console.decisions.filter.outcome"),
                value: "{outcome}",
                onchange: move |event: FormEvent| outcome.set(event.value()),
                option { value: "", {i18n.t("console.decisions.filter.anyOutcome")} }
                for token in ["merged", "queued", "reopened", "withdrawn", "deferred"] {
                    option { key: "{token}", value: "{token}",
                        {i18n.t(&format!("console.decisions.outcome.{token}"))}
                    }
                }
            }
            label { class: "ik-flex", style: "gap:6px;align-items:center;",
                input {
                    r#type: "checkbox",
                    checked: only_blocked,
                    onchange: move |event: FormEvent| blocked_only.set(event.checked()),
                }
                span { {i18n.t("console.decisions.filter.blockedOnly")} }
            }
        }
        StepUpGuard { gate, intro: Some(i18n.t("console.stepUp.intro")) }
        if !notice.read().is_empty() {
            div { class: "ik-note", style: "margin-bottom:10px;", "{notice}" }
        }
        {
            async_view(
                &rows,
                tick.reload(),
                || rsx! { SkeletonRows { count: 6, height: 28 } },
                move |list| {
                    let loaded = list.len();
                    let list: Vec<MergeDecision> = list.iter().filter(|d| keep(d)).cloned().collect();
                    let hits = search_hits(i18n, list.len(), loaded, searching);
                    rsx! {
                        DecisionSearch {
                            placeholder: i18n.t("console.decisions.search.merges"),
                            hits,
                        }
                        if list.is_empty() {
                            div { class: "ik-empty", style: "padding:24px;",
                                {
                                    i18n.t(
                                        if searching {
                                            "console.decisions.searchEmpty"
                                        } else {
                                            "console.decisions.mergeEmpty"
                                        },
                                    )
                                }
                            }
                        } else {
                            div { class: "ik-cons-list",
                                for decision in list {
                                    MergeDecisionRow {
                                        key: "{decision.id}",
                                        decision: Signal::new(decision),
                                        can_revert,
                                        notice,
                                        tick,
                                        gate,
                                    }
                                }
                            }
                        }
                    }
                },
            )
        }
    }
}

/// The journal's search box, wired to the console's own `?q=`.
///
/// Split out because both journals draw it identically and the wording of the hit count is the
/// only thing that differs.
#[component]
fn DecisionSearch(placeholder: String, hits: String) -> Element {
    let nav = use_console_nav();
    rsx! {
        div { style: "margin-bottom:10px;",
            ListSearch {
                placeholder,
                query: nav.query().q,
                on_input: move |text: String| nav.filter(nav.query().with_search(text)),
                hits,
            }
        }
    }
}

/// The hit count beside the search box.
///
/// States what was searched, not just what matched: the endpoint has no text predicate, so this
/// covers the rows the journal has loaded and an operator has to be able to tell that from "the
/// whole journal holds one match".
fn search_hits(i18n: Translator, shown: usize, loaded: usize, searching: bool) -> String {
    if !searching {
        return String::new();
    }
    i18n.args(
        "console.decisions.search.hits",
        &[
            ("count", &shown.to_string()),
            ("loaded", &loaded.to_string()),
        ],
    )
}

/// One merge decision: the headline, the rule, and the evidence behind an expander.
#[component]
fn MergeDecisionRow(
    decision: Signal<MergeDecision>,
    can_revert: bool,
    notice: Signal<String>,
    tick: RefreshTick,
    /// The journal's gate, so a refusal opens the one prompt beside the notice.
    gate: StepUpGate,
) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let mut open = use_signal(|| false);
    let mut busy = use_signal(|| false);
    let mut reason = use_signal(String::new);
    let d = decision.read();
    let expanded = *open.read();
    let id = d.id;

    // Two calls, one shape: the only difference an operator cares about is whether the catalogue
    // is put back, and both write the same durable "not a duplicate" judgement.
    let judge = use_callback(move |revert: bool| {
        let text = reason.peek().trim().to_owned();
        if text.is_empty() {
            notice.set(i18n.t("console.decisions.reasonRequired"));
            return;
        }
        if *busy.peek() {
            return;
        }
        busy.set(true);
        let mut notice = notice;
        spawn(async move {
            let client = gate.client(api);
            let outcome = if revert {
                client
                    .revert_merge_decision()
                    .id(id)
                    .body_map(|body| body.reason(text.clone()))
                    .send()
                    .await
                    .map(|response| {
                        let r = response.into_inner();
                        i18n.args(
                            "console.decisions.reverted",
                            &[("rows", &r.rows_restored.to_string())],
                        )
                    })
            } else {
                client
                    .flag_merge_decision()
                    .id(id)
                    .body_map(|body| body.reason(text.clone()))
                    .send()
                    .await
                    .map(|_| i18n.t("console.decisions.flagged"))
            };
            match outcome {
                Ok(message) => {
                    notice.set(message);
                    reason.set(String::new());
                    tick.bump();
                }
                Err(e) => {
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

    rsx! {
        article { class: "ik-card", style: "padding:12px;margin-bottom:10px;",
            div { class: "ik-flex", style: "gap:10px;align-items:baseline;flex-wrap:wrap;",
                span { class: outcome_tone(&d.outcome),
                    {i18n.t(&format!("console.decisions.outcome.{}", d.outcome))}
                }
                strong { "{d.left_title}" }
                span { class: "ik-muted", "·" }
                strong { "{d.right_title}" }
                span { class: "ik-muted ik-mono", style: "font-size:12px;margin-left:auto;",
                    "{rel_time(i18n, Some(d.decided_at.as_str()))}"
                }
            }
            div { class: "ik-flex", style: "gap:6px;flex-wrap:wrap;margin-top:6px;",
                Pill {
                    {i18n.args("console.merge.score", &[("percent", &percent(d.score))])}
                }
                Pill {
                    {reason_label(i18n, &d.reason)}
                }
                for signal in d.signals.clone() {
                    Pill {
                        tone: Tone::Ghost,
                        key: "{signal}",
                        {signal_label(i18n, &signal)}
                    }
                }
                for guard in d.blocked_by.clone() {
                    Pill {
                        tone: Tone::Caution,
                        key: "blocked-{guard}",
                        {i18n.args("console.decisions.blockedBy", &[("guard", &signal_label(i18n, &guard))])}
                    }
                }
                if d.reverted_at.is_some() {
                    Pill {
                        tone: Tone::Accent,
                        {i18n.t("console.decisions.wasReverted")}
                    }
                }
                if d.flagged_at.is_some() {
                    Pill {
                        tone: Tone::Accent,
                        {i18n.t("console.decisions.wasFlagged")}
                    }
                }
            }
            div { class: "ik-flex", style: "gap:8px;margin-top:8px;flex-wrap:wrap;",
                Button {
                    size: Size::Xs,
                    expanded,
                    on_click: move |_| {
                        let next = !*open.peek();
                        open.set(next);
                    },
                    if expanded {
                        {i18n.t("console.decisions.hideEvidence")}
                    } else {
                        {i18n.t("console.decisions.showEvidence")}
                    }
                }
                if can_revert && d.flagged_at.is_none() {
                    input {
                        class: "ik-input",
                        style: "flex:1;min-width:20ch;",
                        r#type: "text",
                        placeholder: i18n.t("console.decisions.reasonPlaceholder"),
                        "aria-label": i18n.t("console.decisions.reasonPlaceholder"),
                        value: "{reason}",
                        oninput: move |event: FormEvent| reason.set(event.value()),
                    }
                    // Offered only while an undo journal is unspent. A decision that queued a
                    // pair rather than merging one has nothing to put back, and a button that
                    // always errors is worse than no button.
                    if d.revertible {
                        Button {
                            size: Size::Xs,
                            tone: Tone::Accent,
                            disabled: *busy.read(),
                            on_click: move |_| gate.attempt(move || judge.call(true)),
                            {i18n.args(
                            "console.decisions.revert",
                            &[("rows", &d.undo_rows.to_string())],
                            )}
                        }
                    }
                    Button {
                        size: Size::Xs,
                        disabled: *busy.read(),
                        on_click: move |_| gate.attempt(move || judge.call(false)),
                        {i18n.t("console.decisions.flag")}
                    }
                }
            }
            if expanded {
                MergeEvidence { decision }
            }
        }
    }
}

/// The itemised score, the policy in force, and both sides' facts.
#[component]
fn MergeEvidence(decision: Signal<MergeDecision>) -> Element {
    let i18n = use_i18n();
    let d = decision.read();
    let terms = d.terms.as_array().cloned().unwrap_or_default();

    rsx! {
        div { style: "margin-top:10px;border-top:1px solid var(--line);padding-top:10px;",
            h4 { style: "margin:0 0 6px;font-size:13px;", {i18n.t("console.decisions.howScored")} }
            if terms.is_empty() {
                p { class: "ik-muted", {i18n.t("console.decisions.noTerms")} }
            } else {
                div { class: "ik-tablewrap",
                    table { class: "ik-table ik-table-compact",
                        thead {
                            tr {
                                th { {i18n.t("console.decisions.col.rule")} }
                                th { style: "text-align:right;", {i18n.t("console.decisions.col.delta")} }
                                th { {i18n.t("console.decisions.col.detail")} }
                            }
                        }
                        tbody {
                            for (index , term) in terms.iter().enumerate() {
                                tr { key: "{index}",
                                    td {
                                        {term.get("rule").and_then(|v| v.as_str()).map_or_else(
                                            || "?".to_owned(),
                                            |rule| signal_label(i18n, rule),
                                        )}
                                    }
                                    td { class: "ik-mono", style: "text-align:right;",
                                        {signed(term.get("delta").and_then(serde_json::Value::as_f64).unwrap_or(0.0))}
                                    }
                                    td { class: "ik-muted", style: "font-size:12px;",
                                        {term.get("detail").and_then(|v| v.as_str()).unwrap_or("")}
                                    }
                                }
                            }
                            tr {
                                td { strong { {i18n.t("console.decisions.finalScore")} } }
                                td { class: "ik-mono", style: "text-align:right;",
                                    strong { {percent(d.score)} "%" }
                                }
                                td {}
                            }
                        }
                    }
                }
            }
            h4 { style: "margin:12px 0 6px;font-size:13px;", {i18n.t("console.decisions.evidence")} }
            pre { class: "ik-code", style: "max-height:280px;overflow:auto;",
                {pretty(&d.evidence)}
            }
            h4 { style: "margin:12px 0 6px;font-size:13px;", {i18n.t("console.decisions.policyInForce")} }
            pre { class: "ik-code", style: "max-height:180px;overflow:auto;", {pretty(&d.policy)} }
            if let Some(text) = d.revert_reason.clone() {
                p { class: "ik-muted", style: "margin-top:8px;",
                    {i18n.args("console.decisions.revertedBecause", &[("reason", &text)])}
                }
            }
            if let Some(text) = d.flag_reason.clone() {
                p { class: "ik-muted", style: "margin-top:4px;",
                    {i18n.args("console.decisions.flaggedBecause", &[("reason", &text)])}
                }
            }
        }
    }
}

/// The automatic-sync journal, newest first.
#[component]
fn SyncJournal(tick: RefreshTick) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let caps = use_capabilities();
    let nav = use_console_nav();
    let can_revert = caps.can(Permission::SyncRevert);
    let mut action = use_signal(String::new);
    // Default on: a reconciliation is mostly considerations, and an operator opening this panel
    // is almost always asking what actually changed.
    let mut applied_only = use_signal(|| true);
    let notice = use_signal(String::new);
    // See `MergeJournal`: one gate, shared with the rows that report through `notice`.
    let gate = use_step_up_gate();

    let filter_action = action.read().clone();
    let only_applied = *applied_only.read();
    let search = nav.query().q;
    let searching = !search.trim().is_empty();
    let rows = use_resource(use_reactive!(|(filter_action, only_applied, searching)| {
        tick.track();
        let client = api.client();
        async move {
            let depth = if searching {
                SEARCH_PAGE_SIZE
            } else {
                PAGE_SIZE
            };
            let mut request = client.list_sync_decisions().limit(depth);
            if !filter_action.is_empty() {
                request = request.action(filter_action);
            }
            if only_applied {
                request = request.applied(true);
            }
            request
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    }));

    let needle = search.trim().to_lowercase();
    let keep = move |decision: &SyncDecision| {
        needle.is_empty()
            || matches(
                &needle,
                &[
                    decision.series_title.as_deref(),
                    decision.username.as_deref(),
                    Some(decision.provider.as_str()),
                    Some(decision.action.as_str()),
                    Some(decision.reason.as_str()),
                    decision.external_id.as_deref(),
                ],
            )
    };

    rsx! {
        div { class: "ik-flex", style: "gap:8px;flex-wrap:wrap;margin-bottom:10px;",
            select {
                class: "ik-input",
                "aria-label": i18n.t("console.decisions.filter.action"),
                value: "{action}",
                onchange: move |event: FormEvent| action.set(event.value()),
                option { value: "", {i18n.t("console.decisions.filter.anyAction")} }
                for token in ["matched", "unmatched", "pull", "push", "conflict", "skipped", "noop"] {
                    option { key: "{token}", value: "{token}",
                        {i18n.t(&format!("console.decisions.action.{token}"))}
                    }
                }
            }
            label { class: "ik-flex", style: "gap:6px;align-items:center;",
                input {
                    r#type: "checkbox",
                    checked: only_applied,
                    onchange: move |event: FormEvent| applied_only.set(event.checked()),
                }
                span { {i18n.t("console.decisions.filter.appliedOnly")} }
            }
        }
        StepUpGuard { gate, intro: Some(i18n.t("console.stepUp.intro")) }
        if !notice.read().is_empty() {
            div { class: "ik-note", style: "margin-bottom:10px;", "{notice}" }
        }
        {
            async_view(
                &rows,
                tick.reload(),
                || rsx! { SkeletonRows { count: 6, height: 28 } },
                move |list| {
                    let loaded = list.len();
                    let list: Vec<SyncDecision> = list.iter().filter(|d| keep(d)).cloned().collect();
                    let hits = search_hits(i18n, list.len(), loaded, searching);
                    rsx! {
                        DecisionSearch {
                            placeholder: i18n.t("console.decisions.search.sync"),
                            hits,
                        }
                        if list.is_empty() {
                            div { class: "ik-empty", style: "padding:24px;",
                                {
                                    i18n.t(
                                        if searching {
                                            "console.decisions.searchEmpty"
                                        } else {
                                            "console.decisions.syncEmpty"
                                        },
                                    )
                                }
                            }
                        } else {
                            div { class: "ik-cons-list",
                                for decision in list {
                                    SyncDecisionRow {
                                        key: "{decision.id}",
                                        decision: Signal::new(decision),
                                        can_revert,
                                        notice,
                                        tick,
                                        gate,
                                    }
                                }
                            }
                        }
                    }
                },
            )
        }
    }
}

/// One sync decision: what changed on which side, from what to what, and why.
#[component]
fn SyncDecisionRow(
    decision: Signal<SyncDecision>,
    can_revert: bool,
    notice: Signal<String>,
    tick: RefreshTick,
    /// The journal's gate, so a refusal opens the one prompt beside the notice.
    gate: StepUpGate,
) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let mut open = use_signal(|| false);
    let mut busy = use_signal(|| false);
    let mut reason = use_signal(String::new);
    let d = decision.read();
    let expanded = *open.read();
    let id = d.id;
    let title = d
        .series_title
        .clone()
        .unwrap_or_else(|| i18n.t("console.decisions.noSeries"));

    let judge = use_callback(move |revert: bool| {
        let text = reason.peek().trim().to_owned();
        if text.is_empty() {
            notice.set(i18n.t("console.decisions.reasonRequired"));
            return;
        }
        if *busy.peek() {
            return;
        }
        busy.set(true);
        let mut notice = notice;
        spawn(async move {
            let client = gate.client(api);
            let outcome = if revert {
                client
                    .revert_sync_decision()
                    .id(id)
                    .body_map(|body| body.reason(text.clone()))
                    .send()
                    .await
                    .map(|response| {
                        let r = response.into_inner();
                        i18n.args(
                            "console.decisions.syncReverted",
                            &[(
                                "what",
                                &i18n.t(&format!("console.decisions.restored.{}", r.restored)),
                            )],
                        )
                    })
            } else {
                client
                    .flag_sync_decision()
                    .id(id)
                    // Flagging a sync decision wrong almost always means the *match* was wrong,
                    // and a flag that leaves the mapping in place fixes nothing: the next
                    // reconciliation re-derives the same one.
                    .body_map(|body| body.reason(text.clone()).block_match(true))
                    .send()
                    .await
                    .map(|_| i18n.t("console.decisions.flaggedAndBlocked"))
            };
            match outcome {
                Ok(message) => {
                    notice.set(message);
                    reason.set(String::new());
                    tick.bump();
                }
                Err(e) => {
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

    rsx! {
        article { class: "ik-card", style: "padding:12px;margin-bottom:10px;",
            div { class: "ik-flex", style: "gap:10px;align-items:baseline;flex-wrap:wrap;",
                span { class: if d.applied { "ik-pill" } else { "ik-pill ghost" },
                    {i18n.t(&format!("console.decisions.action.{}", d.action))}
                }
                strong { "{title}" }
                Pill {
                    tone: Tone::Ghost,
                    "{d.provider}"
                }
                if let Some(name) = d.username.clone() {
                    span { class: "ik-muted", "{name}" }
                }
                span { class: "ik-muted ik-mono", style: "font-size:12px;margin-left:auto;",
                    "{rel_time(i18n, Some(d.decided_at.as_str()))}"
                }
            }
            div { class: "ik-flex", style: "gap:6px;flex-wrap:wrap;margin-top:6px;",
                Pill {
                    {reason_label(i18n, &d.reason)}
                }
                if let Some(score) = d.match_score {
                    Pill {
                        {i18n.args("console.merge.score", &[("percent", &percent(score))])}
                    }
                }
                if let (Some(before), Some(after)) = (d.local_before.clone(), d.local_after.clone()) {
                    Pill {
                        tone: Tone::Ghost,
                        {i18n.args("console.decisions.localMoved", &[("from", &before), ("to", &after)])}
                    }
                }
                if let (Some(before), Some(after)) = (d.remote_before.clone(), d.remote_after.clone()) {
                    Pill {
                        tone: Tone::Ghost,
                        {i18n.args("console.decisions.remoteMoved", &[("from", &before), ("to", &after)])}
                    }
                }
                if d.reverted_at.is_some() {
                    Pill {
                        tone: Tone::Accent,
                        {i18n.t("console.decisions.wasReverted")}
                    }
                }
                if d.flagged_at.is_some() {
                    Pill {
                        tone: Tone::Accent,
                        {i18n.t("console.decisions.wasFlagged")}
                    }
                }
            }
            div { class: "ik-flex", style: "gap:8px;margin-top:8px;flex-wrap:wrap;",
                Button {
                    size: Size::Xs,
                    expanded,
                    on_click: move |_| {
                        let next = !*open.peek();
                        open.set(next);
                    },
                    if expanded {
                        {i18n.t("console.decisions.hideEvidence")}
                    } else {
                        {i18n.t("console.decisions.showEvidence")}
                    }
                }
                if can_revert && d.flagged_at.is_none() && d.reverted_at.is_none() {
                    input {
                        class: "ik-input",
                        style: "flex:1;min-width:20ch;",
                        r#type: "text",
                        placeholder: i18n.t("console.decisions.reasonPlaceholder"),
                        "aria-label": i18n.t("console.decisions.reasonPlaceholder"),
                        value: "{reason}",
                        oninput: move |event: FormEvent| reason.set(event.value()),
                    }
                    if d.applied {
                        Button {
                            size: Size::Xs,
                            tone: Tone::Accent,
                            disabled: *busy.read(),
                            on_click: move |_| gate.attempt(move || judge.call(true)),
                            {i18n.t("console.decisions.undo")}
                        }
                    }
                    Button {
                        size: Size::Xs,
                        disabled: *busy.read(),
                        on_click: move |_| gate.attempt(move || judge.call(false)),
                        {i18n.t("console.decisions.flagMatch")}
                    }
                }
            }
            if expanded {
                div { style: "margin-top:10px;border-top:1px solid var(--line);padding-top:10px;",
                    dl { class: "ik-kv",
                        DecisionFact { label: i18n.t("console.decisions.ancestorLocal"), value: d.ancestor_local.clone() }
                        DecisionFact { label: i18n.t("console.decisions.ancestorRemote"), value: d.ancestor_remote.clone() }
                        DecisionFact { label: i18n.t("console.decisions.policy"), value: d.policy.clone() }
                        DecisionFact { label: i18n.t("console.decisions.externalId"), value: d.external_id.clone() }
                    }
                    if !d.match_signals.is_empty() {
                        div { class: "ik-flex", style: "gap:6px;flex-wrap:wrap;margin:8px 0;",
                            for signal in d.match_signals.clone() {
                                Pill {
                                    tone: Tone::Ghost,
                                    key: "{signal}",
                                    {signal_label(i18n, &signal)}
                                }
                            }
                        }
                    }
                    pre { class: "ik-code", style: "max-height:280px;overflow:auto;", {pretty(&d.evidence)} }
                    if let Some(text) = d.revert_reason.clone() {
                        p { class: "ik-muted", style: "margin-top:8px;",
                            {i18n.args("console.decisions.revertedBecause", &[("reason", &text)])}
                        }
                    }
                    if let Some(text) = d.flag_reason.clone() {
                        p { class: "ik-muted", style: "margin-top:4px;",
                            {i18n.args("console.decisions.flaggedBecause", &[("reason", &text)])}
                        }
                    }
                }
            }
        }
    }
}

/// One label/value pair, rendered only when there is a value. An empty row in a fact list reads
/// as missing data rather than as "not applicable to this decision".
#[component]
fn DecisionFact(label: String, value: Option<String>) -> Element {
    let Some(value) = value else {
        return rsx! {};
    };
    rsx! {
        dt { "{label}" }
        dd { class: "ik-mono", "{value}" }
    }
}

/// The tone an outcome is drawn in. A merge is the destructive one and has to be findable in a
/// list of hundreds without reading every row.
fn outcome_tone(outcome: &str) -> &'static str {
    match outcome {
        "merged" => "ik-pill vermilion",
        "deferred" => "ik-pill amber",
        _ => "ik-pill",
    }
}

/// The catalogue wording for a decision's reason slug, falling back to the slug.
fn reason_label(i18n: Translator, slug: &str) -> String {
    i18n.t_opt(&format!("console.decisions.reason.{slug}"))
        .unwrap_or_else(|| slug.to_owned())
}

/// A score as a whole-number percentage, matching how the merge queue renders one.
fn percent(score: f32) -> String {
    format!("{:.0}", score * 100.0)
}

/// A score term with its sign always shown: the point of the column is which way each rule moved
/// the number, and a bare `0.10` beside a bare `0.15` hides that one of them was a penalty.
fn signed(delta: f64) -> String {
    format!("{delta:+.3}")
}

/// Pretty-printed JSON, or the value as-is when it will not render.
fn pretty(value: &serde_json::Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}
