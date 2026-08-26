//! One merge in the list: what absorbed what, and whether it can still be taken back.

use crate::i18n::use_i18n;
use crate::models::*;
use crate::util::{rel_time, thousands};
use crate::views::console::decisions::percent;
use dioxus::prelude::*;
use inkstone_ui::{Pill, Tone};

/// A row: the survivor leads, the absorbed title is the subtitle, the facts are the third line.
#[component]
pub(super) fn MergeListRow(
    decision: MergeDecision,
    selected: bool,
    on_pick: EventHandler<uuid::Uuid>,
) -> Element {
    let i18n = use_i18n();
    let id = decision.id;
    let reverted = decision.reverted_at.is_some();
    // Both titles are as they read when the decision was taken, so the pair still reads after
    // one of the two series has stopped existing.
    let (survivor, absorbed) = if decision.absorbed_id == Some(decision.left_id) {
        (decision.right_title.as_str(), decision.left_title.as_str())
    } else {
        (decision.left_title.as_str(), decision.right_title.as_str())
    };
    // What the row's last cell says about the undo journal, which is the fact the operator is
    // scanning for: what is still spendable, what was already spent, and on what.
    let undo = if reverted {
        i18n.args(
            "console.merges.restoredRows",
            &[("rows", &thousands(decision.undo_rows))],
        )
    } else if decision.revertible {
        i18n.args(
            "console.merges.pendingRows",
            &[("rows", &thousands(decision.undo_rows))],
        )
    } else {
        i18n.t("console.merges.undoSpent")
    };

    rsx! {
        button {
            class: match (selected, reverted) {
                (true, _) => "ik-cons-row selected",
                (false, true) => "ik-cons-row dim",
                (false, false) => "ik-cons-row",
            },
            "aria-current": if selected { "true" } else { "false" },
            onclick: move |_| on_pick.call(id),
            div { class: "ik-flex", style: "gap:8px;flex-wrap:wrap;",
                span { style: "font-weight:600;font-size:13px;", "{survivor}" }
                if reverted {
                    Pill { tone: Tone::Accent, class: "ik-pill-tiny", {i18n.t("console.merges.badge.reverted")} }
                }
                if decision.flagged_at.is_some() {
                    Pill { tone: Tone::Caution, class: "ik-pill-tiny", {i18n.t("console.merges.badge.flagged")} }
                }
            }
            div { class: "ik-muted", style: "font-size:12px;margin-top:2px;",
                if reverted {
                    {i18n.t("console.merges.putBack")}
                } else {
                    {i18n.t("console.merges.absorbedVerb")}
                }
                " "
                span { style: "color:var(--text-2);", "{absorbed}" }
            }
            div { class: "ik-meta-line", style: "margin-top:4px;",
                span { class: "ik-mono", {i18n.args("console.merge.score", &[("percent", &percent(decision.score))])} }
                span { {i18n.t(trigger_key(&decision.trigger))} }
                span { {rel_time(i18n, Some(decision.decided_at.as_str()))} }
                span { class: "ik-mono", "{undo}" }
            }
        }
    }
}

/// The catalogue key wording what produced a decision.
///
/// Falls back to `sweep` rather than to the raw token: the vocabulary is the sweep's, and a
/// trigger this build has not been taught is still a sweep of some kind — the one thing it is
/// definitely not is an operator, which is the distinction the row is drawing.
fn trigger_key(trigger: &str) -> &'static str {
    if trigger == "operator" {
        "console.merges.byOperator"
    } else {
        "console.merges.bySweep"
    }
}
