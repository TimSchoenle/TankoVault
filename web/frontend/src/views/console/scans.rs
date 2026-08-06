//! The live scan queue: trigger runs, watch active runs progress, browse recent history,
//! and triage the most recent task failures with their errors.

use crate::api;
use crate::components::async_block;
use crate::i18n::use_i18n;
use crate::models::*;
use crate::util::rel_time;
use crate::views::console::live::ConsoleLive;
use crate::views::console::run_state_pill;
use crate::views::console::RefreshTick;
use dioxus::prelude::*;
use progenitor_client::ResponseValue;

/// Live scan queue: trigger runs, watch active-run progress, browse history, and triage failures.
#[component]
pub(super) fn ScanQueue(tick: RefreshTick) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let mut mode = use_signal(|| ScanMode::Fast);
    let mut message = use_signal(|| Option::<String>::None);

    // Pushed every two seconds by the console stream; this fetch is the first paint and the
    // manual-refresh path, not the cadence. A run in flight changes faster than any poll this
    // panel could justify.
    let live = use_context::<ConsoleLive>();
    let runs = use_resource(move || {
        tick.track();
        let client = api.client();
        async move {
            client
                .list_scans()
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });
    let failures = use_resource(move || {
        tick.track();
        let client = api.client();
        async move {
            client
                .scan_failures()
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });

    let trigger = {
        move |_| {
            let m = *mode.read();
            let tick = tick;
            let _client = api.client();
            spawn(async move {
                let client = api.client();
                let body = TriggerScan {
                    mode: m,
                    provider_id: None,
                };
                match client
                    .trigger_scan()
                    .body(body)
                    .send()
                    .await
                    .map(ResponseValue::into_inner)
                    .map_err(|e| api::friendly_error(i18n, e))
                {
                    // Body carries the planner's run_ids, which this view doesn't render.
                    Ok(_) => {
                        message.set(Some(i18n.t("console.scans.queued")));
                        tick.bump();
                    }
                    Err(e) => message.set(Some(e)),
                }
            });
        }
    };

    rsx! {
        section { class: "ik-tile", style: "margin-bottom:18px;",
            div { class: "ik-flex", style: "justify-content:space-between;flex-wrap:wrap;",
                h3 { style: "margin:0;", {i18n.t("console.scans.title")} }
                div { class: "ik-flex",
                    select {
                        class: "ik-input",
                        style: "width:auto;",
                        value: if *mode.read() == ScanMode::Full { "full" } else { "fast" },
                        onchange: move |e| {
                            mode.set(if e.value() == "full" { ScanMode::Full } else { ScanMode::Fast });
                        },
                        option { value: "fast", {i18n.t("console.scans.modeFast")} }
                        option { value: "full", {i18n.t("console.scans.modeFull")} }
                    }
                    button { class: "ik-btn primary", onclick: trigger,
                        {i18n.t("console.scans.trigger")}
                    }
                }
            }
            if let Some(m) = message.read().clone() {
                p { class: "ik-muted", style: "margin:8px 0 0;", "{m}" }
            }

            // One fetch drives both the active-run strip and the history table: a failed
            // `list_scans` must surface as an error, not render as "no runs" — indistinguishable
            // from a quiet system, the exact wrong conclusion on this screen.
            {
                async_block(
                    &runs,
                    tick.reload(),
                    100,
                    |fetched| {
                        // The stream wins once it has pushed: it is at most two seconds old,
                        // and the fetch behind it is from whenever the panel opened.
                        let pushed = live.runs.read().clone();
                        let all_runs: &Vec<ScanRun> = pushed.as_ref().unwrap_or(fetched);
                        let active: Vec<ScanRun> = all_runs
                            .iter()
                            .filter(|r| matches!(r.state, RunState::Running | RunState::Queued))
                            .cloned()
                            .collect();
                        let all_runs = all_runs.clone();
                        rsx! {
                            div { style: "margin-top:12px;",
                                div { class: "ik-subhead", {i18n.t("console.scans.active")} }
                                if active.is_empty() {
                                    p { class: "ik-muted", style: "font-size:13px;margin:6px 0 0;",
                                        {i18n.t("console.scans.noneActive")}
                                    }
                                } else {
                                    for r in active {
                                        RunProgress { key: "{r.id}", run: Signal::new(r) }
                                    }
                                }
                            }
                            RunHistory { runs: Signal::new(all_runs) }
                        }
                    },
                )
            }
            {
                async_block(
                    &failures,
                    tick.reload(),
                    60,
                    |rows| {
                        let rows = rows.clone();
                        rsx! {
                            FailuresPanel { failures: Signal::new(rows) }
                        }
                    },
                )
            }
        }
    }
}

/// Compact table of the most recent runs (any state).
#[component]
pub(super) fn RunHistory(runs: ReadSignal<Vec<ScanRun>>) -> Element {
    let i18n = use_i18n();
    let list = runs.read();
    if list.is_empty() {
        return rsx! {
            div { style: "margin-top:16px;",
                div { class: "ik-subhead", {i18n.t("console.scans.recent")} }
                p { class: "ik-muted", style: "font-size:13px;margin-left:6px;margin-top:4px;",
                    {i18n.t("console.scans.noRuns")}
                }
            }
        };
    }
    rsx! {
        div { style: "margin-top:16px;",
            div { class: "ik-subhead", {i18n.t("console.scans.recent")} }
            div { class: "ik-tablewrap",
                table { class: "ik-table ik-table-compact",
                    thead {
                        tr {
                            th { {i18n.t("console.scans.col.state")} }
                            th { {i18n.t("console.scans.col.mode")} }
                            th { {i18n.t("console.scans.col.scope")} }
                            th { {i18n.t("console.scans.col.progress")} }
                            th { {i18n.t("console.scans.col.started")} }
                            th { {i18n.t("console.scans.col.finished")} }
                        }
                    }
                    tbody {
                        for r in list.iter().cloned() {
                            RunHistoryRow { key: "{r.id}", run: Signal::new(r) }
                        }
                    }
                }
            }
        }
    }
}

#[component]
pub(super) fn RunHistoryRow(run: Signal<ScanRun>) -> Element {
    let i18n = use_i18n();
    let r = run.read();
    let pill = run_state_pill(r.state);
    let label = i18n.t(r.state.label_key());
    #[expect(
        clippy::cast_possible_truncation,
        reason = "`progress()` returns a 0..=1 ratio and the clamp makes the percentage total"
    )]
    let pct = (r.progress() * 100.0).round().clamp(0.0, 100.0) as i32;
    let scope = match &r.provider_id {
        Some(ScanRunProviderId::Variant1(id)) => format!("#{}", &id.to_string()[..8]),
        Some(ScanRunProviderId::Variant0(v)) => v
            .as_str()
            .map_or_else(|| i18n.t("console.scans.scopeUnknown"), str::to_owned),
        None => i18n.t("console.scans.scopeAll"),
    };
    rsx! {
        tr {
            td { span { class: "{pill}", "{label}" } }
            td { class: "ik-mono", "{r.mode:?}" }
            td { class: "ik-mono ik-muted", "{scope}" }
            td {
                div { class: "ik-flex", style: "gap:8px;",
                    span { class: "ik-mono", style: "font-size:12px;min-width:82px;",
                        "{r.done_tasks}/{r.total_tasks}"
                        if r.failed_tasks > 0 {
                            span { style: "color:var(--vermilion);", " ·{r.failed_tasks}✗" }
                        }
                    }
                    div { class: "ik-progress", style: "flex:1;min-width:60px;",
                        span { style: "width:{pct}%;" }
                    }
                }
            }
            td { class: "ik-muted ik-mono", style: "font-size:12px;",
                "{rel_time(i18n, r.started_at.as_deref())}"
            }
            td { class: "ik-muted ik-mono", style: "font-size:12px;",
                "{rel_time(i18n, r.finished_at.as_deref())}"
            }
        }
    }
}

/// Recent task failures with their errors — the operator's triage feed.
#[component]
pub(super) fn FailuresPanel(failures: Signal<Vec<FailedTask>>) -> Element {
    let i18n = use_i18n();
    let list = failures.read();
    if list.is_empty() {
        return rsx! {
            div { style: "margin-top:16px;",
                div { class: "ik-subhead", {i18n.t("console.scans.failures")} }
                p { class: "ik-muted", style: "font-size:13px;margin:6px 0 0;",
                    {i18n.t("console.scans.noFailures")}
                }
            }
        };
    }
    rsx! {
        div { style: "margin-top:16px;",
            div { class: "ik-subhead", {i18n.t("console.scans.failures")} }
            div { style: "margin-top:8px;display:grid;gap:8px;",
                for f in list.iter().cloned() {
                    div { key: "{f.id}", class: "ik-fail",
                        div { class: "ik-flex", style: "justify-content:space-between;gap:10px;",
                            div { class: "ik-flex", style: "gap:8px;flex-wrap:wrap;",
                                span { class: "ik-pill vermilion", "{f.kind}" }
                                span { class: "ik-mono ik-muted", style: "font-size:12px;",
                                    {
                                        let slug = f
                                            .provider_slug
                                            .clone()
                                            .unwrap_or_else(|| i18n.t("time.unknown"));
                                        i18n.args(
                                            "console.scans.failureMeta",
                                            &[
                                                ("provider", &slug),
                                                ("mode", &format!("{:?}", f.mode)),
                                                ("attempts", &f.attempts.to_string()),
                                            ],
                                        )
                                    }
                                }
                            }
                            span { class: "ik-muted ik-mono", style: "font-size:12px;",
                                "{rel_time(i18n, f.finished_at.as_deref())}"
                            }
                        }
                        p { class: "ik-mono", style: "margin:6px 0 0;font-size:12px;color:var(--vermilion);word-break:break-word;",
                            {f.error.clone().unwrap_or_else(|| i18n.t("console.scans.noErrorMessage"))}
                        }
                    }
                }
            }
        }
    }
}

#[component]
pub(super) fn RunProgress(run: Signal<ScanRun>) -> Element {
    let i18n = use_i18n();
    let r = run.read();
    #[expect(
        clippy::cast_possible_truncation,
        reason = "`progress()` returns a 0..=1 ratio and the clamp makes the percentage total"
    )]
    let pct = (r.progress() * 100.0).round().clamp(0.0, 100.0) as i32;
    let width = format!("width:{pct}%;");
    rsx! {
        div { style: "margin-top:12px;",
            div { class: "ik-flex", style: "justify-content:space-between;font-size:13px;",
                span { "{i18n.t(r.state.label_key())} · {r.mode:?}" }
                span { class: "ik-mono",
                    {
                        i18n.args(
                            "console.scans.progress",
                            &[
                                ("done", &r.done_tasks.to_string()),
                                ("total", &r.total_tasks.to_string()),
                                ("failed", &r.failed_tasks.to_string()),
                            ],
                        )
                    }
                }
            }
            div { class: "ik-progress", style: "margin-top:6px;", span { style: "{width}" } }
        }
    }
}
