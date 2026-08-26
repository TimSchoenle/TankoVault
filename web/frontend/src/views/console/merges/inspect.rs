//! The merge inspector: why the pair scored the way it did on the left, what a revert would put
//! back on the right, and the two writes that end the question underneath.
//!
//! The layout is the argument. An operator deciding whether to unmerge is weighing evidence
//! against cost, and those are the two columns — the old screen made them two expanders that
//! could not be open at once.

use super::unmerge::UnmergeBlock;
use crate::components::{Section, StepUpGate, TabBar, TabKind};
use crate::i18n::use_i18n;
use crate::models::*;
use crate::util::{rel_time, thousands};
use crate::views::console::decisions::{percent, pretty, reason_label, signed};
use crate::views::console::{signal_label, use_console_nav, RefreshTick};
use crate::Route;
use dioxus::prelude::*;
use inkstone_ui::{button_class, Pill, Size, Tone};

/// The inspector's tab strip.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Why,
    Evidence,
    Trail,
}

impl TabKind for Tab {
    fn all() -> &'static [Self] {
        &[Self::Why, Self::Evidence, Self::Trail]
    }

    fn label_key(self) -> &'static str {
        match self {
            Self::Why => "console.merges.tab.why",
            Self::Evidence => "console.merges.tab.evidence",
            Self::Trail => "console.merges.tab.trail",
        }
    }
}

impl Tab {
    /// This tab's `?tab=` token.
    const fn token(self) -> &'static str {
        match self {
            Self::Why => "why",
            Self::Evidence => "evidence",
            Self::Trail => "trail",
        }
    }

    /// An unrecognised token opens the default tab rather than refusing the link.
    fn parse(token: &str) -> Self {
        <Self as TabKind>::all()
            .iter()
            .copied()
            .find(|tab| tab.token() == token)
            .unwrap_or(Self::Why)
    }
}

/// Which of the pair survived and which stopped existing, with both titles as they read then.
struct Sides<'a> {
    survivor_id: Option<SeriesId>,
    survivor_title: &'a str,
    absorbed_id: Option<SeriesId>,
    absorbed_title: &'a str,
}

impl<'a> Sides<'a> {
    fn of(decision: &'a MergeDecision) -> Self {
        if decision.absorbed_id == Some(decision.left_id) {
            Self {
                survivor_id: decision.survivor_id,
                survivor_title: &decision.right_title,
                absorbed_id: decision.absorbed_id,
                absorbed_title: &decision.left_title,
            }
        } else {
            Self {
                survivor_id: decision.survivor_id,
                survivor_title: &decision.left_title,
                absorbed_id: decision.absorbed_id,
                absorbed_title: &decision.right_title,
            }
        }
    }
}

/// The whole right-hand pane for one merge.
#[component]
pub(super) fn MergeInspector(
    decision: MergeDecision,
    gate: StepUpGate,
    tick: RefreshTick,
) -> Element {
    let i18n = use_i18n();
    let nav = use_console_nav();
    let tab = Tab::parse(nav.query().tab_token());
    let sides = Sides::of(&decision);
    let reverted = decision.reverted_at.is_some();

    rsx! {
        div { class: "ik-cons-insphead",
            div { class: "ik-flex", style: "align-items:flex-start;gap:14px;",
                div { style: "min-width:0;flex:1;",
                    div { class: "ik-flex", style: "gap:10px;flex-wrap:wrap;",
                        h2 { class: "ik-insp-title", "{sides.survivor_title}" }
                        if reverted {
                            Pill { tone: Tone::Accent, {i18n.t("console.merges.badge.reverted")} }
                        } else {
                            Pill { {i18n.t("console.merges.badge.merged")} }
                        }
                        if decision.flagged_at.is_some() {
                            Pill { tone: Tone::Caution, {i18n.t("console.merges.badge.flagged")} }
                        }
                    }
                    p { class: "ik-muted", style: "font-size:13px;margin:6px 0 0;",
                        {i18n.t("console.merges.absorbedVerb")}
                        " "
                        span { style: "color:var(--text-2);", "{sides.absorbed_title}" }
                        if let Some(id) = sides.absorbed_id {
                            " "
                            span { class: "ik-mono", style: "font-size:11.5px;color:var(--faint);", "{id}" }
                            " "
                            span { style: "font-size:12px;",
                                {
                                    i18n.t(
                                        if reverted {
                                            "console.merges.idResolvesAgain"
                                        } else {
                                            "console.merges.idGone"
                                        },
                                    )
                                }
                            }
                        }
                    }
                    div { class: "ik-meta-line",
                        span { class: "ik-mono", {i18n.args("console.merges.mergeId", &[("id", &decision.id.to_string())])} }
                        span { {rel_time(i18n, Some(decision.decided_at.as_str()))} }
                        span { {reason_label(i18n, &decision.reason)} }
                        span { class: "ik-mono", {i18n.args("console.merge.score", &[("percent", &percent(decision.score))])} }
                    }
                }
                if let Some(id) = sides.survivor_id {
                    Link {
                        to: Route::Series { id: id.to_string() },
                        class: button_class(Tone::Neutral, Size::Sm, false),
                        style: "flex:none;",
                        {i18n.t("console.merges.openSeries")}
                    }
                }
            }
            TabBar {
                selected: tab,
                flush: true,
                on_select: move |next: Tab| nav.select(nav.query().with_tab(next.token())),
            }
        }
        match tab {
            Tab::Why => rsx! {
                div { class: "ik-cons-inspbody",
                    div { style: "min-width:0;",
                        ScoreTable { decision: decision.clone() }
                        BothRecords { decision: decision.clone() }
                    }
                    div { style: "min-width:0;",
                        UndoInventory { decision: decision.clone() }
                        UnmergeBlock { decision: decision.clone(), gate, tick }
                    }
                }
            },
            Tab::Evidence => rsx! {
                div { class: "ik-cons-inspbody", style: "grid-template-columns:1fr;",
                    div { style: "min-width:0;",
                        Section { label: i18n.t("console.decisions.evidence"),
                            pre { class: "ik-code", style: "max-height:420px;overflow:auto;",
                                {pretty(&decision.evidence)}
                            }
                        }
                        Section { label: i18n.t("console.decisions.policyInForce"),
                            pre { class: "ik-code", style: "max-height:280px;overflow:auto;",
                                {pretty(&decision.policy)}
                            }
                        }
                    }
                }
            },
            Tab::Trail => rsx! {
                div { class: "ik-cons-inspbody", style: "grid-template-columns:1fr;",
                    div { style: "min-width:0;",
                        AuditTrail { decision: decision.clone() }
                    }
                }
            },
        }
    }
}

/// The itemised score: one row per rule, then the total against the threshold that judged it.
#[component]
fn ScoreTable(decision: MergeDecision) -> Element {
    let i18n = use_i18n();
    let terms = decision.terms.as_array().cloned().unwrap_or_default();
    let threshold = decision
        .policy
        .get("auto_merge")
        .and_then(serde_json::Value::as_f64);

    rsx! {
        Section { label: i18n.t("console.decisions.howScored"),
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
                                    strong { {percent(decision.score)} "%" }
                                }
                                td { class: "ik-muted", style: "font-size:12px;",
                                    if let Some(threshold) = threshold {
                                        {i18n.args("console.merges.threshold", &[("value", &format!("{threshold:.2}"))])}
                                    }
                                }
                            }
                        }
                    }
                }
            }
            div { class: "ik-flex", style: "gap:6px;flex-wrap:wrap;margin-top:8px;",
                for signal in decision.signals.clone() {
                    Pill { tone: Tone::Ghost, key: "{signal}", {signal_label(i18n, &signal)} }
                }
                for guard in decision.blocked_by.clone() {
                    Pill {
                        tone: Tone::Caution,
                        key: "blocked-{guard}",
                        {i18n.args("console.decisions.blockedBy", &[("guard", &signal_label(i18n, &guard))])}
                    }
                }
            }
        }
    }
}

/// Both series as the sweep saw them, from the evidence the decision stored.
///
/// Read out of `evidence` rather than fetched: the absorbed id no longer resolves, so there is
/// nothing left to fetch — and the numbers that mattered are the ones at merge time anyway.
#[component]
fn BothRecords(decision: MergeDecision) -> Element {
    let i18n = use_i18n();
    let sides = Sides::of(&decision);
    let survivor = sides.survivor_id.and_then(|id| side_facts(&decision, id));
    let absorbed = sides.absorbed_id.and_then(|id| side_facts(&decision, id));
    if survivor.is_none() && absorbed.is_none() {
        return rsx! {};
    }

    rsx! {
        Section { label: i18n.t("console.merges.bothRecords"),
            div { class: "ik-flex", style: "gap:10px;align-items:stretch;",
                if let Some(facts) = survivor {
                    RecordCard { role: i18n.t("console.merges.kept"), facts, kept: true }
                }
                if let Some(facts) = absorbed {
                    RecordCard { role: i18n.t("console.merges.absorbedRole"), facts, kept: false }
                }
            }
        }
    }
}

/// One side of the pair, as the journal recorded it.
#[derive(Clone, PartialEq)]
struct SideFacts {
    id: SeriesId,
    title: String,
    release_year: Option<i64>,
    sources: Option<i64>,
    chapters: Option<i64>,
    watchers: Option<i64>,
}

/// Pull one side out of the stored evidence by id, when the journal recorded it.
fn side_facts(decision: &MergeDecision, id: SeriesId) -> Option<SideFacts> {
    ["left", "right"]
        .into_iter()
        .filter_map(|key| decision.evidence.get(key))
        .find(|side| {
            side.get("id")
                .and_then(|v| v.as_str())
                .is_some_and(|found| found == id.to_string())
        })
        .map(|side| SideFacts {
            id,
            title: side
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_owned(),
            release_year: side.get("release_year").and_then(serde_json::Value::as_i64),
            sources: side.get("sources").and_then(serde_json::Value::as_i64),
            chapters: side.get("chapters").and_then(serde_json::Value::as_i64),
            watchers: side.get("watchers").and_then(serde_json::Value::as_i64),
        })
}

#[component]
fn RecordCard(role: String, facts: SideFacts, kept: bool) -> Element {
    let i18n = use_i18n();
    let identity = match facts.release_year {
        Some(year) => format!("{} · {year}", facts.id),
        None => facts.id.to_string(),
    };

    rsx! {
        div {
            class: "ik-card",
            style: "flex:1;min-width:0;padding:11px 12px;",
            div { class: "ik-sec-lbl", style: if kept { "color:var(--acc3);" } else { "" }, "{role}" }
            div { style: "font-weight:600;font-size:13px;margin-top:5px;", "{facts.title}" }
            div { class: "ik-mono", style: "font-size:10.5px;color:var(--faint);margin-top:2px;",
                "{identity}"
            }
            div { class: "ik-meta-line", style: "margin-top:7px;",
                if let Some(count) = facts.sources {
                    span { {i18n.plural("console.merges.sourceTally", count, &[])} }
                }
                if let Some(count) = facts.chapters {
                    span { {i18n.plural("console.merges.chapterTally", count, &[])} }
                }
                if let Some(count) = facts.watchers {
                    span { {i18n.plural("console.merges.watcherTally", count, &[])} }
                }
            }
        }
    }
}

/// What a revert would write, itemised by the table it would write to.
///
/// The total is the row's `undo_rows`; the itemisation is `undo_breakdown`, and both come from
/// the stored journal rather than from a count of what exists now — a revert restores what was
/// captured, not what has accumulated since.
#[component]
fn UndoInventory(decision: MergeDecision) -> Element {
    let i18n = use_i18n();

    rsx! {
        Section {
            label: i18n.t("console.merges.putsBack"),
            trailing: rsx! {
                if decision.revertible {
                    Pill { tone: Tone::Positive, class: "ik-pill-tiny", {i18n.t("console.merges.journalIntact")} }
                } else {
                    Pill { class: "ik-pill-tiny", {i18n.t("console.merges.undoSpent")} }
                }
            },
            if decision.undo_breakdown.is_empty() {
                p { class: "ik-muted", style: "font-size:12.5px;margin:0;",
                    {i18n.t("console.merges.noInventory")}
                }
            } else {
                div { class: "ik-tablewrap",
                    table { class: "ik-table ik-table-compact",
                        tbody {
                            for segment in decision.undo_breakdown.clone() {
                                tr { key: "{segment.kind}",
                                    td { {segment_label(i18n, &segment.kind)} }
                                    td { class: "ik-mono", style: "text-align:right;",
                                        "{thousands(segment.rows)}"
                                    }
                                }
                            }
                            tr {
                                td { strong { {i18n.t("console.merges.restoredTotal")} } }
                                td { class: "ik-mono", style: "text-align:right;",
                                    strong { "{thousands(decision.undo_rows)}" }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Who did what to this decision, and what they gave as the reason.
#[component]
fn AuditTrail(decision: MergeDecision) -> Element {
    let i18n = use_i18n();

    rsx! {
        div { class: "ik-timeline",
            div {
                span { class: "val", {i18n.t("console.merges.trail.decided")} }
                " · "
                {rel_time(i18n, Some(decision.decided_at.as_str()))}
                " · "
                {i18n.t(if decision.trigger == "operator" {
                    "console.merges.byOperator"
                } else {
                    "console.merges.bySweep"
                })}
            }
            if let Some(sweep) = decision.sweep_id {
                div {
                    span { class: "val", {i18n.t("console.merges.trail.sweep")} }
                    " · {sweep}"
                }
            }
            if let Some(at) = decision.flagged_at.clone() {
                div {
                    span { class: "val", {i18n.t("console.merges.trail.flagged")} }
                    " · "
                    {rel_time(i18n, Some(at.as_str()))}
                    if let Some(reason) = decision.flag_reason.clone() {
                        " · {reason}"
                    }
                }
            }
            if let Some(at) = decision.reverted_at.clone() {
                div {
                    span { class: "val", {i18n.t("console.merges.trail.reverted")} }
                    " · "
                    {rel_time(i18n, Some(at.as_str()))}
                    if let Some(reason) = decision.revert_reason.clone() {
                        " · {reason}"
                    }
                }
            }
        }
    }
}

/// The catalogue wording for an undo-journal segment, falling back to the key.
///
/// Falling back rather than failing is deliberate: the journal's shape is the merge code's, and
/// a segment added there must not render as a missing-translation string in the console — see
/// [`signal_label`], which takes the same position for the scorer's vocabulary.
fn segment_label(i18n: crate::i18n::Translator, kind: &str) -> String {
    i18n.t_opt(&format!("console.merges.segment.{kind}"))
        .unwrap_or_else(|| kind.to_owned())
}
