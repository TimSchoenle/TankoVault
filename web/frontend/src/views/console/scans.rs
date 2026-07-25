//! The live scan queue: trigger runs, watch active runs progress, browse recent history,
//! and triage the most recent task failures with their errors.

use crate::api;
use crate::models::*;
use crate::state::use_session;
use crate::util::rel_time;
use crate::views::console::run_state_pill;
use crate::views::console::RefreshTick;
use dioxus::prelude::*;
use progenitor_client::ResponseValue;

/// Live scan queue: trigger a global run, watch every active run's progress, browse recent
/// run history, and triage the most recent task failures with their errors. Auto-refreshes
/// on the shared console tick.
#[component]
pub(super) fn ScanQueue(tick: RefreshTick) -> Element {
    let api = api::use_api();
    let session = use_session();
    let mut mode = use_signal(|| ScanMode::Fast);
    let mut message = use_signal(|| Option::<String>::None);

    let runs = {
        use_resource(move || {
            tick.track();
            let client = api.client();
            async move {
                if session.is_authenticated() {
                    Some(
                        client
                            .list_scans()
                            .send()
                            .await
                            .map(ResponseValue::into_inner)
                            .map_err(api::friendly_error),
                    )
                } else {
                    None
                }
            }
        })
    };
    let failures = {
        use_resource(move || {
            tick.track();
            let client = api.client();
            async move {
                if session.is_authenticated() {
                    Some(
                        client
                            .scan_failures()
                            .send()
                            .await
                            .map(ResponseValue::into_inner)
                            .map_err(api::friendly_error),
                    )
                } else {
                    None
                }
            }
        })
    };

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
                    .map_err(api::friendly_error)
                {
                    Ok(()) => {
                        message.set(Some("Scan queued for all providers.".to_owned()));
                        tick.bump();
                    }
                    Err(e) => message.set(Some(e)),
                }
            });
        }
    };

    let all_runs = match &*runs.read_unchecked() {
        Some(Some(Ok(list))) => list.clone(),
        _ => Vec::new(),
    };
    let active: Vec<ScanRun> = all_runs
        .iter()
        .filter(|r| matches!(r.state, RunState::Running | RunState::Queued))
        .cloned()
        .collect();

    rsx! {
        section { class: "ik-tile", style: "margin-bottom:18px;",
            div { class: "ik-flex", style: "justify-content:space-between;flex-wrap:wrap;",
                h3 { style: "margin:0;", "Scan queue" }
                div { class: "ik-flex",
                    select {
                        class: "ik-input",
                        style: "width:auto;",
                        value: if *mode.read() == ScanMode::Full { "full" } else { "fast" },
                        onchange: move |e| {
                            mode.set(if e.value() == "full" { ScanMode::Full } else { ScanMode::Fast });
                        },
                        option { value: "fast", "Fast scan (new chapters)" }
                        option { value: "full", "Full scan (rebuild)" }
                    }
                    button { class: "ik-btn primary", onclick: trigger, "Trigger scan (all)" }
                }
            }
            if let Some(m) = message.read().clone() {
                p { class: "ik-muted", style: "margin:8px 0 0;", "{m}" }
            }

            div { style: "margin-top:12px;",
                div { class: "ik-subhead", "Active runs" }
                if active.is_empty() {
                    p { class: "ik-muted", style: "font-size:13px;margin:6px 0 0;", "No runs in flight." }
                } else {
                    for r in active {
                        RunProgress { key: "{r.id}", run: Signal::new(r) }
                    }
                }
            }

            RunHistory { runs: Signal::new(all_runs) }
            FailuresPanel { failures: Signal::new(match &*failures.read_unchecked() {
                Some(Some(Ok(list))) => list.clone(),
                _ => Vec::new(),
            }) }
        }
    }
}

/// Compact table of the most recent runs (any state).
#[component]
pub(super) fn RunHistory(runs: ReadSignal<Vec<ScanRun>>) -> Element {
    let list = runs.read();
    if list.is_empty() {
        return rsx! {
            div { style: "margin-top:16px;",
                div { class: "ik-subhead", "Recent runs" }
                p { class: "ik-muted", style: "font-size:13px;margin-left:6px;margin-top:4px;", "No scan runs yet." }
            }
        };
    }
    rsx! {
        div { style: "margin-top:16px;",
            div { class: "ik-subhead", "Recent runs" }
            div { class: "ik-tablewrap",
                table { class: "ik-table ik-table-compact",
                    thead {
                        tr {
                            th { "State" }
                            th { "Mode" }
                            th { "Scope" }
                            th { "Progress" }
                            th { "Started" }
                            th { "Finished" }
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
    let r = run.read();
    let (pill, label) = run_state_pill(r.state);
    // `progress()` returns a 0..=1 ratio, so the rounded percentage is always in range.
    #[allow(clippy::cast_possible_truncation)]
    let pct = (r.progress() * 100.0).round().clamp(0.0, 100.0) as i32;
    let scope = match &r.provider_id {
        Some(ScanRunProviderId::Variant1(id)) => format!("#{}", &id.to_string()[..8]),
        Some(ScanRunProviderId::Variant0(v)) => v.as_str().unwrap_or("unknown").to_owned(),
        None => "all providers".to_owned(),
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
            td { class: "ik-muted ik-mono", style: "font-size:12px;", "{rel_time(r.started_at.as_deref())}" }
            td { class: "ik-muted ik-mono", style: "font-size:12px;", "{rel_time(r.finished_at.as_deref())}" }
        }
    }
}

/// Recent task failures with their errors — the operator's triage feed.
#[component]
pub(super) fn FailuresPanel(failures: Signal<Vec<FailedTask>>) -> Element {
    let list = failures.read();
    if list.is_empty() {
        return rsx! {
            div { style: "margin-top:16px;",
                div { class: "ik-subhead", "Recent failures" }
                p { class: "ik-muted", style: "font-size:13px;margin:6px 0 0;", "No task failures recorded. Clean." }
            }
        };
    }
    rsx! {
        div { style: "margin-top:16px;",
            div { class: "ik-subhead", "Recent failures" }
            div { style: "margin-top:8px;display:grid;gap:8px;",
                for f in list.iter().cloned() {
                    div { key: "{f.id}", class: "ik-fail",
                        div { class: "ik-flex", style: "justify-content:space-between;gap:10px;",
                            div { class: "ik-flex", style: "gap:8px;flex-wrap:wrap;",
                                span { class: "ik-pill vermilion", "{f.kind}" }
                                span { class: "ik-mono ik-muted", style: "font-size:12px;",
                                    "{f.provider_slug.clone().unwrap_or_else(|| \"—\".to_owned())} · {f.mode:?} · attempt {f.attempts}"
                                }
                            }
                            span { class: "ik-muted ik-mono", style: "font-size:12px;", "{rel_time(f.finished_at.as_deref())}" }
                        }
                        p { class: "ik-mono", style: "margin:6px 0 0;font-size:12px;color:var(--vermilion);word-break:break-word;",
                            "{f.error.clone().unwrap_or_else(|| \"(no error message)\".to_owned())}"
                        }
                    }
                }
            }
        }
    }
}

#[component]
pub(super) fn RunProgress(run: Signal<ScanRun>) -> Element {
    let r = run.read();
    // `progress()` returns a 0..=1 ratio, so the rounded percentage is always in range.
    #[allow(clippy::cast_possible_truncation)]
    let pct = (r.progress() * 100.0).round().clamp(0.0, 100.0) as i32;
    let width = format!("width:{pct}%;");
    rsx! {
        div { style: "margin-top:12px;",
            div { class: "ik-flex", style: "justify-content:space-between;font-size:13px;",
                span { "{r.state.label()} · {r.mode:?}" }
                span { class: "ik-mono", "{r.done_tasks}/{r.total_tasks} ({r.failed_tasks} failed)" }
            }
            div { class: "ik-progress", style: "margin-top:6px;", span { style: "{width}" } }
        }
    }
}
