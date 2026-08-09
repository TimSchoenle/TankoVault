//! What is happening *right now*: one card per run in flight, and a tail of the tasks that have
//! just settled.
//!
//! Run counters alone cannot tell "working" from "wedged" — both leave `done_tasks` where it
//! was. The figures that can are here: how many tasks a worker is holding, how long the oldest
//! claim has been held, how many workers are on it, and what settled in the last few seconds.

use super::{age_seconds, duration_label, elapsed_seconds, percent, scope_label};
use crate::i18n::use_i18n;
use crate::models::{
    RunActivity, RunState, RunStateExt as _, ScanActivity, ScanRun, TaskEvent, TaskState,
    TaskStateExt as _,
};
use crate::util::rel_time;
use crate::views::console::run_state_pill;
use dioxus::prelude::*;

/// How long a single claim may sit before the card says so. Deliberately generous: a full
/// catalogue page against a slow provider legitimately takes minutes, and crying wolf at one
/// would train an operator to ignore the one that matters.
const STALE_CLAIM_SECONDS: f64 = 300.0;

/// The runs in flight, with their task-level state, and the tail of what has just settled.
#[component]
pub(super) fn LivePanel(runs: Vec<ScanRun>, activity: Option<ScanActivity>) -> Element {
    let i18n = use_i18n();
    let active: Vec<ScanRun> = runs
        .into_iter()
        .filter(|run| matches!(run.state, RunState::Running | RunState::Queued))
        .collect();
    let (per_run, events) = activity.map_or_else(
        || (Vec::new(), Vec::new()),
        |activity| (activity.runs, activity.events),
    );

    rsx! {
        div { style: "margin-top:16px;",
            div { class: "ik-subhead", {i18n.t("console.scans.active")} }
            if active.is_empty() {
                p { class: "ik-muted", style: "font-size:13px;margin:6px 0 0;",
                    {i18n.t("console.scans.noneActive")}
                }
            } else {
                div { style: "display:grid;gap:10px;margin-top:8px;",
                    for run in active {
                        RunCard {
                            key: "{run.id}",
                            activity: per_run.iter().find(|a| a.run_id == run.id).cloned(),
                            run,
                        }
                    }
                }
            }
            ActivityTail { events }
        }
    }
}

/// One in-flight run: progress split by outcome, plus the figures that say whether it is moving.
#[component]
fn RunCard(run: ScanRun, activity: Option<RunActivity>) -> Element {
    let i18n = use_i18n();
    let done_pct = if run.total_tasks > 0 {
        percent(f64::from(run.done_tasks) / f64::from(run.total_tasks))
    } else {
        0
    };
    let failed_pct = if run.total_tasks > 0 {
        percent(f64::from(run.failed_tasks) / f64::from(run.total_tasks))
    } else {
        0
    };
    let elapsed = elapsed_seconds(&run);
    let elapsed_label = elapsed.map_or_else(|| i18n.t("time.unknown"), |s| duration_label(i18n, s));

    // Settled tasks per minute, and from it the time the rest would take at the same pace. Both
    // are `None` until something has actually settled — an ETA extrapolated from nothing is a
    // number an operator would plan around and should not.
    let settled = f64::from(run.done_tasks + run.failed_tasks);
    let rate = elapsed
        .filter(|seconds| *seconds > 0.0 && settled > 0.0)
        .map(|seconds| settled / (seconds / 60.0));
    let rate_label = rate.map_or_else(
        || i18n.t("time.unknown"),
        |value| {
            i18n.args(
                "console.scan.detail.perMinute",
                &[("count", &format!("{value:.1}"))],
            )
        },
    );
    let remaining = f64::from(run.total_tasks) - settled;
    let eta_label = rate
        .filter(|value| *value > 0.0 && remaining > 0.0)
        .map_or_else(
            || i18n.t("time.unknown"),
            |value| duration_label(i18n, remaining / value * 60.0),
        );

    let in_flight = activity.as_ref().map_or(0, |a| a.running_tasks);
    let queued = activity.as_ref().map_or(0, |a| a.queued_tasks);
    let workers = activity.as_ref().map_or(0, |a| a.workers);
    let kinds = activity
        .as_ref()
        .map(|a| a.kinds.join(", "))
        .filter(|kinds| !kinds.is_empty());
    let stale_claim = activity
        .as_ref()
        .and_then(|a| age_seconds(a.oldest_claim_at.as_deref()))
        .filter(|age| *age >= STALE_CLAIM_SECONDS);

    rsx! {
        div { class: "ik-tile", style: "padding:12px;",
            div { class: "ik-flex", style: "justify-content:space-between;gap:10px;flex-wrap:wrap;",
                div { class: "ik-flex", style: "gap:8px;flex-wrap:wrap;",
                    span { class: run_state_pill(run.state), {i18n.t(run.state.label_key())} }
                    span { class: "ik-mono", style: "font-size:12px;", "{run.mode:?}" }
                    span { class: "ik-mono ik-muted", style: "font-size:12px;",
                        "{scope_label(i18n, &run)}"
                    }
                }
                span { class: "ik-mono", style: "font-size:12px;",
                    {
                        i18n.args(
                            "console.scans.progress",
                            &[
                                ("done", &run.done_tasks.to_string()),
                                ("total", &run.total_tasks.to_string()),
                                ("failed", &run.failed_tasks.to_string()),
                            ],
                        )
                    }
                }
            }
            // Two segments, not one: a bar that fills to 100% on a run where every task failed
            // is the single most misleading thing this panel could draw.
            div { class: "ik-progress split", style: "margin-top:8px;",
                span { style: "width:{done_pct}%;" }
                span { class: "fail", style: "width:{failed_pct}%;" }
            }
            div { class: "ik-flex", style: "gap:14px;flex-wrap:wrap;margin-top:8px;",
                Figure { label: i18n.t("console.scan.live.inFlight"), value: in_flight.to_string() }
                Figure { label: i18n.t("console.scan.live.queued"), value: queued.to_string() }
                Figure { label: i18n.t("console.scan.live.workers"), value: workers.to_string() }
                Figure { label: i18n.t("console.scan.detail.elapsed"), value: elapsed_label }
                Figure { label: i18n.t("console.scan.detail.throughput"), value: rate_label }
                Figure { label: i18n.t("console.scan.live.eta"), value: eta_label }
                if let Some(kinds) = kinds {
                    Figure { label: i18n.t("console.scan.live.kinds"), value: kinds }
                }
            }
            if let Some(age) = stale_claim {
                p { style: "margin:8px 0 0;font-size:12px;color:var(--star-ink);",
                    {
                        i18n.args(
                            "console.scan.live.staleClaim",
                            &[("duration", &duration_label(i18n, age))],
                        )
                    }
                }
            }
        }
    }
}

/// One labelled figure on a run card.
#[component]
fn Figure(label: String, value: String) -> Element {
    rsx! {
        div {
            span { class: "ik-muted", style: "font-size:11px;display:block;", "{label}" }
            span { class: "ik-mono", style: "font-size:13px;", "{value}" }
        }
    }
}

/// The tasks that settled most recently, newest first.
///
/// Scoped server-side to the runs in flight, so it goes quiet when nothing is running rather
/// than replaying the last scan forever — an empty tail beside an empty active list is the
/// honest picture of an idle deployment.
#[component]
fn ActivityTail(events: Vec<TaskEvent>) -> Element {
    let i18n = use_i18n();
    if events.is_empty() {
        return rsx! {};
    }
    rsx! {
        div { style: "margin-top:14px;",
            div { class: "ik-subhead", {i18n.t("console.scan.live.tail")} }
            div { class: "ik-tail",
                for event in events {
                    div { key: "{event.id}", class: "row",
                        span {
                            class: "dot",
                            style: match event.state {
                                TaskState::Failed => "background:var(--vermilion);",
                                TaskState::Done => "background:var(--jade-bright);",
                                _ => "background:var(--muted);",
                            },
                        }
                        span { class: "ik-mono ik-muted", style: "font-size:11.5px;min-width:7ch;",
                            "{rel_time(i18n, event.finished_at.as_deref())}"
                        }
                        span { class: "ik-mono", style: "font-size:12px;", "{event.kind}" }
                        span { class: "ik-mono ik-muted", style: "font-size:11.5px;",
                            {event.provider_slug.clone().unwrap_or_default()}
                        }
                        span { class: "tgt ik-mono", style: "font-size:11.5px;",
                            "{target_label(&event.target)}"
                        }
                        if event.state.is_failure() {
                            span { style: "font-size:11.5px;color:var(--vermilion);",
                                {
                                    event
                                        .error
                                        .clone()
                                        .unwrap_or_else(|| i18n.t("console.scan.failures.noError"))
                                }
                            }
                        } else {
                            span { class: "ik-muted", style: "font-size:11.5px;",
                                {i18n.t(event.state.label_key())}
                            }
                        }
                    }
                }
            }
        }
    }
}

/// What a task was pointed at, in one short string.
///
/// The planner writes an object whose shape depends on the task kind (`{"path":…,"page":…}` for
/// a catalogue walk, `{"series_id":…}` for a series). Rendering the raw JSON would fill the tail
/// with braces, so the two fields an operator actually reads are pulled out and everything else
/// falls back to the compact JSON it already is.
fn target_label(target: &serde_json::Value) -> String {
    let path = target.get("path").and_then(serde_json::Value::as_str);
    let page = target.get("page").and_then(serde_json::Value::as_i64);
    match (path, page) {
        (Some(path), Some(page)) => format!("{path} #{page}"),
        (Some(path), None) => path.to_owned(),
        (None, Some(page)) => format!("#{page}"),
        (None, None) => {
            let raw = target.to_string();
            raw.chars().take(60).collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::target_label;
    use serde_json::json;

    /// The tail is one line per task; a target rendered as raw JSON pushes the error message —
    /// the only part an operator is reading for — off the end of the row.
    #[test]
    fn a_target_renders_as_a_path_not_as_json() {
        assert_eq!(
            target_label(&json!({ "path": "/manga/x", "page": 3 })),
            "/manga/x #3"
        );
        assert_eq!(target_label(&json!({ "path": "/manga/x" })), "/manga/x");
        assert_eq!(target_label(&json!({ "page": 12 })), "#12");
    }

    /// An unrecognised shape still says *something*, truncated rather than unbounded.
    #[test]
    fn an_unknown_target_falls_back_to_bounded_json() {
        let label = target_label(&json!({ "series_id": "018f4c2a-0000-7000-8000-000000000001" }));
        assert!(label.starts_with('{'));
        assert!(label.chars().count() <= 60);
    }
}
