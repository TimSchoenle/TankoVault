//! The live scan queue: trigger runs, watch active runs progress, browse recent history,
//! and triage the most recent task failures with their errors.

use crate::api;
use crate::components::async_block;
use crate::hooks::use_reload;
use crate::i18n::use_i18n;
use crate::models::*;
use crate::util::rel_time;
use crate::views::console::live::ConsoleLive;
use crate::views::console::query::Window as TimeWindow;
use crate::views::console::run_state_pill;
use crate::views::console::{use_console_nav, ConsoleQuery, RefreshTick};
use crate::wire::types::{RunState as WireRunState, ScanRunId};
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
    let nav = use_console_nav();
    let view = nav.query();
    let state_token = view.status.clone();
    let since = view.since;
    // One clone per resource: `use_reactive!` moves its dependencies, and all three of these
    // narrow by the same provider slug.
    let provider_for_runs = view.provider.clone();
    let provider_for_failures = view.provider.clone();
    let provider_for_groups = view.provider.clone();

    let runs = use_resource(use_reactive!(|(provider_for_runs, state_token, since)| {
        let provider = provider_for_runs;
        tick.track();
        let client = api.client();
        async move {
            let mut request = client.list_scans();
            if let Some(slug) = provider.as_deref() {
                request = request.provider(slug);
            }
            if let Some(state) = state_token.as_deref().and_then(parse_state) {
                request = request.state(state);
            }
            if let Some(from) = since.since_iso() {
                request = request.since(from);
            }
            request
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    }));

    // Grouping is a *view* of the same failures, not a different set, so it stays a local
    // toggle: it changes nothing an operator would want to send someone.
    let mut grouped = use_signal(|| true);
    let failures = use_resource(use_reactive!(|(provider_for_failures, since)| {
        let provider = provider_for_failures;
        tick.track();
        let client = api.client();
        async move {
            let mut request = client.scan_failures();
            if let Some(slug) = provider.as_deref() {
                request = request.provider(slug);
            }
            if let Some(from) = since.since_iso() {
                request = request.since(from);
            }
            request
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    }));
    let groups = use_resource(use_reactive!(|(provider_for_groups, since)| {
        let provider = provider_for_groups;
        tick.track();
        let client = api.client();
        async move {
            let mut request = client.scan_failure_groups();
            if let Some(slug) = provider.as_deref() {
                request = request.provider(slug);
            }
            if let Some(from) = since.since_iso() {
                request = request.since(from);
            }
            request
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    }));

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
            div { class: "ik-flex", style: "gap:8px;flex-wrap:wrap;margin-top:10px;",
                select {
                    class: "ik-input",
                    style: "width:auto;",
                    "aria-label": i18n.t("console.scan.filter.state"),
                    value: view.status.clone().unwrap_or_default(),
                    onchange: move |event: FormEvent| {
                        let chosen = event.value();
                        nav.filter(ConsoleQuery {
                            status: (!chosen.is_empty()).then_some(chosen),
                            ..nav.query()
                        });
                    },
                    option { value: "", {i18n.t("console.scan.filter.anyState")} }
                    for state in STATE_FILTERS {
                        option {
                            key: "{state.0}",
                            value: "{state.0}",
                            {i18n.t(state.1.label_key())}
                        }
                    }
                }
                select {
                    class: "ik-input",
                    style: "width:auto;",
                    "aria-label": i18n.t("console.audit.filter.window"),
                    value: view.since.token(),
                    onchange: move |event: FormEvent| {
                        nav.filter(ConsoleQuery {
                            since: TimeWindow::parse_token(&event.value()),
                            ..nav.query()
                        });
                    },
                    for option in TimeWindow::ALL {
                        option {
                            key: "{option.label_key()}",
                            value: option.token(),
                            {i18n.t(option.label_key())}
                        }
                    }
                }
                input {
                    class: "ik-input",
                    style: "width:auto;flex:1;min-width:12ch;",
                    r#type: "search",
                    placeholder: i18n.t("console.scan.filter.provider"),
                    "aria-label": i18n.t("console.scan.filter.provider"),
                    value: view.provider.clone().unwrap_or_default(),
                    oninput: move |event: FormEvent| {
                        let slug = event.value();
                        nav.filter(ConsoleQuery {
                            provider: (!slug.trim().is_empty()).then_some(slug),
                            ..nav.query()
                        });
                    },
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
                        let all_runs: &Vec<ScanRun> = pushed.as_ref().unwrap_or(&fetched.items);
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
                            RunDrawer {}
                            RunHistory { runs: Signal::new(all_runs) }
                        }
                    },
                )
            }
            div { class: "ik-flex", style: "justify-content:space-between;align-items:center;margin-top:16px;",
                div { class: "ik-subhead", style: "margin:0;", {i18n.t("console.scans.failures")} }
                label { class: "ik-flex", style: "gap:5px;font-size:12px;align-items:center;",
                    input {
                        r#type: "checkbox",
                        checked: *grouped.read(),
                        onchange: move |event: FormEvent| grouped.set(event.checked()),
                    }
                    {i18n.t("console.scan.failures.grouped")}
                }
            }
            if *grouped.read() {
                {
                    async_block(
                        &groups,
                        tick.reload(),
                        60,
                        |rows| {
                            let rows = rows.clone();
                            rsx! {
                                GroupedFailures { groups: Signal::new(rows) }
                            }
                        },
                    )
                }
            } else {
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
}

/// The selected run, fetched by id.
///
/// `get_scan` has been in the generated client since the console shipped and was called by
/// nothing, so a run was a row that could not be opened. Fetched rather than picked out of the
/// list, because a run linked to from elsewhere may be older than the window the list holds.
#[component]
fn RunDrawer() -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let nav = use_console_nav();
    let reload = use_reload();
    let selected = nav.query().sel;

    let detail = use_resource(use_reactive!(|selected| {
        reload.track();
        let client = api.client();
        async move {
            let Some(id) = selected
                .as_deref()
                .and_then(|id| id.parse::<ScanRunId>().ok())
            else {
                return Ok(None);
            };
            client
                .get_scan()
                .run_id(id)
                .send()
                .await
                .map(|response| Some(response.into_inner()))
                .map_err(|e| api::friendly_error(i18n, e))
        }
    }));

    let Some(Ok(Some(run))) = detail.read().clone() else {
        return rsx! {};
    };

    let elapsed = elapsed_label(i18n, &run);
    rsx! {
        div { class: "ik-tile", style: "margin-top:12px;",
            div { class: "ik-flex", style: "justify-content:space-between;align-items:center;gap:10px;",
                div { class: "ik-flex", style: "gap:8px;align-items:center;",
                    span { class: run_state_pill(run.state), {i18n.t(run.state.label_key())} }
                    span { class: "ik-mono", style: "font-size:12px;", "{run.mode:?}" }
                    span { class: "ik-mono ik-muted", style: "font-size:11.5px;", "{run.id}" }
                }
                button {
                    class: "ik-btn xs",
                    onclick: move |_| nav.select(nav.query().with_selection(None)),
                    {i18n.t("common.close")}
                }
            }
            div { class: "ik-kv", style: "margin-top:10px;",
                Fact {
                    label: i18n.t("console.scan.detail.tasks"),
                    value: format!("{}/{}", run.done_tasks, run.total_tasks),
                }
                Fact {
                    label: i18n.t("console.scans.col.state"),
                    value: i18n.t(run.state.label_key()),
                }
                Fact { label: i18n.t("console.scan.detail.elapsed"), value: elapsed.0 }
                Fact { label: i18n.t("console.scan.detail.throughput"), value: elapsed.1 }
            }
        }
    }
}

/// One labelled figure in the drawer.
#[component]
fn Fact(label: String, value: String) -> Element {
    rsx! {
        div {
            span { class: "ik-muted", style: "font-size:11px;display:block;", "{label}" }
            span { class: "ik-mono", style: "font-size:13px;", "{value}" }
        }
    }
}

/// A run's elapsed time and its task throughput, both already worded.
///
/// Reads the clock only for a run still in flight: a finished run's duration is the difference
/// between its own two stamps, and using `now` there would make a completed run's elapsed time
/// keep growing on screen.
fn elapsed_label(i18n: crate::i18n::Translator, run: &ScanRun) -> (String, String) {
    const MS_PER_MINUTE: f64 = 60_000.0;
    let unknown = i18n.t("time.unknown");
    let Some(started) = run.started_at.as_deref().and_then(parse_ms) else {
        return (unknown.clone(), unknown);
    };
    let ended = run
        .finished_at
        .as_deref()
        .and_then(parse_ms)
        .unwrap_or_else(crate::platform::now_ms);
    let minutes = ((ended - started) / MS_PER_MINUTE).max(0.0);

    let elapsed = if minutes < 1.0 {
        i18n.t("console.scan.detail.underAMinute")
    } else {
        i18n.args(
            "console.scan.detail.minutes",
            &[("count", &format!("{minutes:.0}"))],
        )
    };
    let per_minute = if minutes < f64::EPSILON {
        unknown
    } else {
        let rate = f64::from(run.done_tasks) / minutes;
        i18n.args(
            "console.scan.detail.perMinute",
            &[("count", &format!("{rate:.1}"))],
        )
    };
    (elapsed, per_minute)
}

/// An RFC 3339 stamp as epoch milliseconds, or `None` if it cannot be parsed.
fn parse_ms(stamp: &str) -> Option<f64> {
    let ms = crate::platform::parse_timestamp_ms(stamp);
    ms.is_finite().then_some(ms)
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
    let nav = use_console_nav();
    let r = run.read();
    let run_id = r.id;
    let selected = nav.query().sel.as_deref() == Some(run_id.to_string().as_str());
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
            class: if selected { "ik-cons-row selected" } else { "ik-cons-row" },
            tabindex: "0",
            role: "button",
            "aria-selected": if selected { "true" } else { "false" },
            onclick: move |_| {
                // Toggle, so clicking the open run closes the drawer rather than doing nothing.
                let next = if selected { None } else { Some(run_id.to_string()) };
                nav.select(nav.query().with_selection(next));
            },
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

/// The run states the filter offers, paired with the `?status=` token each sends.
///
/// Hand-listed rather than derived: the wire vocabulary is generated, but which states are
/// worth *filtering by* is a product decision — `queued` and `running` are the two an operator
/// watches, `failed` is the one they hunt.
const STATE_FILTERS: [(&str, WireRunState); 5] = [
    ("queued", WireRunState::Queued),
    ("running", WireRunState::Running),
    ("completed", WireRunState::Completed),
    ("failed", WireRunState::Failed),
    ("cancelled", WireRunState::Cancelled),
];

/// Parse a `?status=` token into the run state it names.
fn parse_state(token: &str) -> Option<WireRunState> {
    STATE_FILTERS
        .iter()
        .find(|(name, _)| *name == token)
        .map(|(_, state)| *state)
}

/// Failures collapsed by their error text, worst first.
///
/// One broken selector that hit twelve series is one problem. The flat feed presents it as
/// twelve rows of the same sentence, and on a bad day that is the entire feed.
#[component]
fn GroupedFailures(groups: Signal<Vec<crate::wire::types::FailureGroupView>>) -> Element {
    let i18n = use_i18n();
    let list = groups.read();
    if list.is_empty() {
        return rsx! {
            p { class: "ik-muted", style: "font-size:13px;margin:6px 0 0;",
                {i18n.t("console.scans.noFailures")}
            }
        };
    }
    rsx! {
        div { style: "margin-top:8px;display:grid;gap:8px;",
            for group in list.iter().cloned() {
                div { key: "{group.error:?}", class: "ik-fail",
                    div { class: "ik-flex", style: "justify-content:space-between;gap:10px;",
                        div { class: "ik-flex", style: "gap:8px;flex-wrap:wrap;",
                            span { class: "ik-pill vermilion", "{group.count}×" }
                            for slug in group.providers.clone() {
                                span { key: "{slug}", class: "ik-pill", "{slug}" }
                            }
                        }
                        span { class: "ik-muted ik-mono", style: "font-size:12px;",
                            "{rel_time(i18n, group.latest_at.as_deref())}"
                        }
                    }
                    p {
                        class: "ik-mono",
                        style: "margin:6px 0 0;font-size:12px;color:var(--vermilion);word-break:break-word;",
                        {
                            group
                                .error
                                .clone()
                                .unwrap_or_else(|| i18n.t("console.scan.failures.noError"))
                        }
                    }
                }
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
            p { class: "ik-muted", style: "font-size:13px;margin:6px 0 0;",
                {i18n.t("console.scans.noFailures")}
            }
        };
    }
    rsx! {
        div {
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
