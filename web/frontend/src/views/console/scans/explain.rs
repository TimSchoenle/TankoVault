//! "Why is this taking so long?", answered from the run's own timing breakdown.
//!
//! Opened per run, on demand: the breakdown is two aggregates and up to a hundred task rows, and
//! nobody wants it for the runs that are behaving. What it shows, in order, is the verdict — one
//! sentence naming the cause — then where the time went by stage, then the individual tasks that
//! cost the most, so the sentence can be checked rather than taken on faith.

use super::activity::target_label;
use super::duration_label;
use super::stages::{percent_of, stage_label, word, Verdict};
use crate::api;
use crate::i18n::use_i18n;
use crate::models::{ScanRunDetail, ScanRunId, ScanTaskDetail};
use dioxus::prelude::*;
use progenitor_client::ResponseValue;

/// How many task rows the panel lists. The API already caps and orders costliest-first, so this
/// is only about how much of that page is worth reading.
const TASK_ROWS: usize = 8;

/// The breakdown of one run, fetched when the panel opens.
#[component]
pub(super) fn ExplainPanel(run_id: ScanRunId) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();

    let detail = use_resource(use_reactive!(|run_id| {
        let client = api.client();
        async move {
            client
                .scan_run_detail()
                .run_id(run_id)
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    }));

    rsx! {
        div { class: "ik-tile", style: "padding:10px;margin-top:8px;background:transparent;",
            match &*detail.read_unchecked() {
                None => rsx! {
                    p { class: "ik-muted", style: "margin:0;font-size:12px;",
                        {i18n.t("common.loading")}
                    }
                },
                Some(Err(message)) => rsx! {
                    p { style: "margin:0;font-size:12px;color:var(--vermilion);", "{message}" }
                },
                Some(Ok(detail)) => rsx! { Breakdown { detail: detail.clone() } },
            }
        }
    }
}

/// The verdict, the stage split, and the tasks behind them.
#[component]
fn Breakdown(detail: ScanRunDetail) -> Element {
    let i18n = use_i18n();
    let telemetry = &detail.telemetry;
    let verdict = Verdict::of(telemetry, &detail.stages);

    rsx! {
        p { style: "margin:0 0 10px;font-size:12.5px;", {word(i18n, &verdict)} }

        div { class: "ik-flex", style: "gap:14px;flex-wrap:wrap;margin-bottom:10px;",
            Stat {
                label: i18n.t("console.scan.explain.busy"),
                value: duration_label(i18n, millis_to_seconds(telemetry.busy_ms)),
            }
            Stat {
                label: i18n.t("console.scan.explain.waiting"),
                value: duration_label(i18n, millis_to_seconds(telemetry.wait_ms)),
            }
            Stat {
                label: i18n.t("console.scan.explain.paceWait"),
                value: duration_label(i18n, millis_to_seconds(telemetry.pace_wait_ms)),
            }
            Stat {
                label: i18n.t("console.scan.explain.requests"),
                value: telemetry.requests.to_string(),
            }
            if telemetry.solver_calls > 0 {
                Stat {
                    label: i18n.t("console.scan.explain.solves"),
                    value: telemetry.solver_calls.to_string(),
                }
            }
            if telemetry.throttled > 0 {
                Stat {
                    label: i18n.t("console.scan.explain.throttledCount"),
                    value: telemetry.throttled.to_string(),
                }
            }
        }

        if detail.stages.is_empty() {
            p { class: "ik-muted", style: "margin:0;font-size:12px;",
                {i18n.t("console.scan.explain.noStages")}
            }
        } else {
            div { class: "ik-subhead", {i18n.t("console.scan.explain.byStage")} }
            div { style: "display:grid;gap:4px;margin:6px 0 10px;",
                for total in detail.stages.iter() {
                    div {
                        key: "{total.stage}",
                        class: "ik-flex",
                        style: "gap:8px;align-items:center;",
                        span { class: "ik-mono", style: "font-size:11.5px;min-width:16ch;",
                            "{stage_label(i18n, &total.stage)}"
                        }
                        // The bar is the point: a stage list without one is eight numbers an
                        // operator has to divide in their head to read at all.
                        div {
                            class: "ik-progress",
                            style: "flex:1;min-width:80px;",
                            span {
                                style: "width:{percent_of(total.millis, telemetry.busy_ms)}%;",
                            }
                        }
                        span { class: "ik-mono ik-muted", style: "font-size:11.5px;min-width:7ch;",
                            {duration_label(i18n, millis_to_seconds(total.millis))}
                        }
                    }
                }
            }
        }

        if !detail.tasks.is_empty() {
            div { class: "ik-subhead", {i18n.t("console.scan.explain.slowest")} }
            div { class: "ik-tail", style: "margin-top:4px;",
                for task in detail.tasks.iter().take(TASK_ROWS) {
                    TaskRow { key: "{task.id}", task: task.clone() }
                }
            }
        }
    }
}

/// One task line: what it was pointed at, which stage it is in or ended in, and what it cost.
#[component]
fn TaskRow(task: ScanTaskDetail) -> Element {
    let i18n = use_i18n();
    rsx! {
        div { class: "row",
            span { class: "ik-mono", style: "font-size:11.5px;min-width:9ch;", "{task.kind}" }
            span { class: "tgt ik-mono", style: "font-size:11.5px;",
                "{target_label(&task.target)}"
            }
            if let Some(stage) = task.stage.as_deref() {
                span { class: "ik-mono ik-muted", style: "font-size:11.5px;",
                    "{stage_label(i18n, stage)}"
                }
            }
            span { class: "ik-mono", style: "font-size:11.5px;min-width:6ch;",
                {
                    task.duration_ms
                        .map_or_else(
                            || i18n.t("time.unknown"),
                            |ms| duration_label(i18n, millis_to_seconds(i64::from(ms))),
                        )
                }
            }
            if let Some(error) = task.error.as_deref() {
                span { style: "font-size:11.5px;color:var(--vermilion);", "{error}" }
            }
        }
    }
}

/// One labelled figure.
#[component]
fn Stat(label: String, value: String) -> Element {
    rsx! {
        div {
            span { class: "ik-muted", style: "font-size:11px;display:block;", "{label}" }
            span { class: "ik-mono", style: "font-size:13px;", "{value}" }
        }
    }
}

/// Milliseconds as the seconds [`duration_label`] takes.
#[expect(
    clippy::cast_precision_loss,
    reason = "a millisecond count large enough to lose f64 precision is 285,000 years"
)]
fn millis_to_seconds(millis: i64) -> f64 {
    millis.max(0) as f64 / 1_000.0
}
