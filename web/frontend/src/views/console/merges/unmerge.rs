//! The two ways to say a merge was wrong: put the catalogue back, or record the judgement and
//! leave it merged.
//!
//! They are deliberately separate, and both write the same durable "not a duplicate" — a merge
//! can be correct and still worth undoing as a precaution, and a merge can be wrong and no longer
//! worth the disruption of unpicking. Whichever is taken, the suppression is what stops the next
//! sweep re-making the same decision; a bare revert would simply be re-merged.

use crate::api;
use crate::components::StepUpGate;
use crate::i18n::use_i18n;
use crate::models::*;
use crate::state::capabilities::use_capabilities;
use crate::util::{rel_time, thousands};
use crate::views::console::RefreshTick;
use crate::wire::types::Permission;
use dioxus::prelude::*;
use inkstone_ui::{Button, Size, Tone};

/// The decision block: what each outcome does, the reason both demand, and the buttons.
#[component]
pub(super) fn UnmergeBlock(
    decision: MergeDecision,
    gate: StepUpGate,
    tick: RefreshTick,
) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let caps = use_capabilities();
    let mut reason = use_signal(String::new);
    let mut busy = use_signal(|| false);
    let mut notice = use_signal(String::new);
    let id = decision.id;

    // Two calls, one shape: the only difference an operator cares about is whether the catalogue
    // is put back, and both record the same judgement.
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
                        i18n.args(
                            "console.decisions.reverted",
                            &[("rows", &response.into_inner().rows_restored.to_string())],
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

    // Already settled: a decision that has been reverted or flagged has had its judgement
    // recorded, and offering the same two buttons again would write a second one.
    if let Some(settled) = settled_line(i18n, &decision) {
        return rsx! {
            div { class: "ik-note", style: "margin-top:16px;", "{settled}" }
        };
    }
    if !caps.can(Permission::MergeRevert) {
        return rsx! {};
    }

    rsx! {
        div {
            style: "margin-top:16px;border:1px solid color-mix(in srgb, var(--acc) 40%, transparent);\
                    border-radius:var(--radius);overflow:hidden;",
            div { style: "padding:13px;background:color-mix(in srgb, var(--acc) 7%, transparent);",
                p { style: "font-size:12.5px;line-height:1.6;margin:0 0 10px;color:var(--text-2);",
                    if decision.revertible {
                        {
                            i18n.args(
                                "console.merges.unmergeBody",
                                &[("rows", &thousands(decision.undo_rows))],
                            )
                        }
                    } else {
                        {i18n.t("console.merges.spentBody")}
                    }
                }
                label { style: "display:block;",
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
                    if decision.revertible {
                        Button {
                            size: Size::Sm,
                            tone: Tone::Accent,
                            disabled: *busy.read(),
                            on_click: move |_| gate.attempt(move || judge.call(true)),
                            {
                                i18n.args(
                                    "console.merges.unmergeCta",
                                    &[("rows", &thousands(decision.undo_rows))],
                                )
                            }
                        }
                    }
                    Button {
                        size: Size::Sm,
                        disabled: *busy.read(),
                        on_click: move |_| gate.attempt(move || judge.call(false)),
                        {i18n.t("console.merges.flagCta")}
                    }
                }
                if !notice.read().is_empty() {
                    div { class: "ik-note", style: "margin-top:10px;", "{notice}" }
                }
            }
            div { class: "ik-listfoot", {i18n.t("console.merges.suppressionNote")} }
        }
    }
}

/// The one-line record of a decision that has already been judged, or `None` while it stands.
fn settled_line(i18n: crate::i18n::Translator, decision: &MergeDecision) -> Option<String> {
    let said = |reason: Option<&String>| {
        reason
            .filter(|text| !text.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| i18n.t("console.merges.noReason"))
    };
    if let Some(at) = decision.reverted_at.as_deref() {
        return Some(i18n.args(
            "console.merges.wasReverted",
            &[
                ("when", &rel_time(i18n, Some(at))),
                ("reason", &said(decision.revert_reason.as_ref())),
            ],
        ));
    }
    if let Some(at) = decision.flagged_at.as_deref() {
        return Some(i18n.args(
            "console.merges.wasFlagged",
            &[
                ("when", &rel_time(i18n, Some(at))),
                ("reason", &said(decision.flag_reason.as_ref())),
            ],
        ));
    }
    None
}
