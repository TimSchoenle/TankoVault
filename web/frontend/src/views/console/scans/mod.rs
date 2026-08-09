//! The scan queue: trigger runs, read the window's health, watch what is in flight right now,
//! browse the history, and triage — or clear — the failures.
//!
//! # The filter is the panel
//!
//! Every control here writes to the URL (see [`super::query`]) and every fetch reads it back, so
//! the same filter narrows the health figures, the history and the failure feed at once. That
//! was the defect this module was rebuilt around: the panel *had* filters, but the live stream
//! pushed an unfiltered top-twenty over the top of the filtered fetch, so a narrowed filter
//! changed nothing an operator could see. [`merge_runs`] is where the two are reconciled now,
//! and it is the one place that decides which source wins.

mod activity;
mod explain;
mod failures;
mod filters;
mod health;
mod history;
mod stages;

use crate::api;
use crate::components::{async_block, use_step_up_gate, StepUpPrompt};
use crate::i18n::use_i18n;
use crate::models::*;
use crate::views::console::live::ConsoleLive;
use crate::views::console::query::Window as TimeWindow;
use crate::views::console::{use_console_nav, ConsoleQuery, RefreshTick};
use crate::wire::types::RunState as WireRunState;
use dioxus::prelude::*;
use progenitor_client::ResponseValue;

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

/// The filter as every fetch on this panel reads it — resolved once from the URL rather than
/// re-parsed per resource, so the history, the health figures and the failure feed cannot end up
/// answering three slightly different questions.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ScanFilter {
    provider: Option<String>,
    status: Option<String>,
    mode: Option<String>,
    sort: String,
    cleared: bool,
    since: TimeWindow,
}

impl ScanFilter {
    fn of(query: &ConsoleQuery) -> Self {
        Self {
            provider: query
                .provider
                .as_deref()
                .map(str::trim)
                .filter(|slug| !slug.is_empty())
                .map(str::to_owned),
            status: query.status.clone(),
            mode: query.mode.clone(),
            sort: query
                .sort
                .clone()
                .unwrap_or_else(|| RunSort::Recent.token().to_owned()),
            cleared: query.cleared,
            since: query.since,
        }
    }

    fn run_state(&self) -> Option<WireRunState> {
        self.status.as_deref().and_then(parse_state)
    }

    fn scan_mode(&self) -> Option<ScanMode> {
        self.mode.as_deref().and_then(ScanModeExt::parse)
    }

    fn ordering(&self) -> RunSort {
        RunSortExt::parse(&self.sort)
    }

    /// Whether anything narrows the run list. Decides whether the live push may be shown whole
    /// — see [`merge_runs`].
    fn narrows_runs(&self) -> bool {
        self.provider.is_some()
            || self.run_state().is_some()
            || self.scan_mode().is_some()
            || self.since != TimeWindow::Any
            || self.ordering() != RunSort::Recent
    }
}

/// Reconcile the fetched page with the live push.
///
/// The stream carries an unfiltered, unsorted top-twenty by creation, which is the *right*
/// answer only when nothing narrows the list. Under any filter the fetch is authoritative and
/// the push is used solely to refresh the counters of rows the fetch already returned — so a
/// run's progress still moves live, and the filter still means what it says.
///
/// Showing the push wholesale under a filter is the bug this replaces: it silently answered a
/// different question from the one the operator asked, with no indication that it had.
fn merge_runs(fetched: &[ScanRun], pushed: Option<&Vec<ScanRun>>, filtered: bool) -> Vec<ScanRun> {
    let Some(pushed) = pushed else {
        return fetched.to_vec();
    };
    if !filtered {
        return pushed.clone();
    }
    fetched
        .iter()
        .map(|row| {
            pushed
                .iter()
                .find(|fresh| fresh.id == row.id)
                .cloned()
                .unwrap_or_else(|| row.clone())
        })
        .collect()
}

/// Live scan queue: trigger runs, read the window's health, watch what is in flight, browse the
/// history and triage failures.
#[component]
pub(in crate::views::console) fn ScanQueue(tick: RefreshTick) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let live = use_context::<ConsoleLive>();
    let nav = use_console_nav();
    let gate = use_step_up_gate();

    let mut mode = use_signal(|| ScanMode::Fast);
    let mut message = use_signal(|| Option::<String>::None);

    let filter = ScanFilter::of(&nav.query());
    // One clone per resource: `use_reactive!` moves its dependencies, and every fetch on this
    // panel is narrowed by the same filter.
    let filter_for_runs = filter.clone();
    let filter_for_summary = filter.clone();
    let filter_for_failures = filter.clone();
    let filter_for_groups = filter.clone();

    let runs = use_resource(use_reactive!(|filter_for_runs| {
        let filter = filter_for_runs;
        tick.track();
        let client = api.client();
        async move {
            let mut request = client.list_scans().sort(filter.ordering());
            if let Some(slug) = filter.provider.as_deref() {
                request = request.provider(slug);
            }
            if let Some(state) = filter.run_state() {
                request = request.state(state);
            }
            if let Some(scan_mode) = filter.scan_mode() {
                request = request.mode(scan_mode);
            }
            if let Some(from) = filter.since.since_iso() {
                request = request.since(from);
            }
            request
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    }));

    let summary = use_resource(use_reactive!(|filter_for_summary| {
        let filter = filter_for_summary;
        tick.track();
        let client = api.client();
        async move {
            let mut request = client.scan_summary();
            if let Some(slug) = filter.provider.as_deref() {
                request = request.provider(slug);
            }
            if let Some(from) = filter.since.since_iso() {
                request = request.since(from);
            }
            request
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    }));

    // Seeds the live panel: the stream's first `activity` event is three seconds out, and a tail
    // that starts empty reads as an idle deployment rather than as one that has not answered yet.
    let activity_seed = use_resource(move || {
        tick.track();
        let client = api.client();
        async move {
            client
                .scan_activity()
                .send()
                .await
                .map(ResponseValue::into_inner)
                .ok()
        }
    });

    let failures = use_resource(use_reactive!(|filter_for_failures| {
        let filter = filter_for_failures;
        tick.track();
        let client = api.client();
        async move {
            let mut request = client.scan_failures().include_cleared(filter.cleared);
            if let Some(slug) = filter.provider.as_deref() {
                request = request.provider(slug);
            }
            if let Some(from) = filter.since.since_iso() {
                request = request.since(from);
            }
            request
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    }));

    let groups = use_resource(use_reactive!(|filter_for_groups| {
        let filter = filter_for_groups;
        tick.track();
        let client = api.client();
        async move {
            let mut request = client.scan_failure_groups().include_cleared(filter.cleared);
            if let Some(slug) = filter.provider.as_deref() {
                request = request.provider(slug);
            }
            if let Some(from) = filter.since.since_iso() {
                request = request.since(from);
            }
            request
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    }));

    let trigger = move |_| {
        let chosen = *mode.read();
        // Elevated: triggering a run is a mutating operator capability, which the API answers
        // `403 step_up_required` to until it has a second factor.
        let client = gate.client(api);
        spawn(async move {
            let body = TriggerScan {
                mode: chosen,
                provider_id: None,
            };
            match client.trigger_scan().body(body).send().await {
                // Body carries the planner's run_ids, which this view doesn't render.
                Ok(_) => {
                    message.set(Some(i18n.t("console.scans.queued")));
                    tick.bump();
                }
                Err(e) => {
                    if !gate.refused(api::Refusal::of(&e)) {
                        message.set(Some(api::guarded_error(i18n, e)));
                    }
                }
            }
        });
    };

    // Narrowed by the panel's own provider filter, so "stop the queue" means what the operator is
    // looking at: with a provider selected it drains that provider, and with none it drains
    // everything. A button that always stopped everything would be unusable on the one screen
    // that exists to narrow to a single misbehaving provider.
    let drain_provider = filter.provider.clone();
    let drain = move |_| {
        let provider = drain_provider.clone();
        let client = gate.client(api);
        spawn(async move {
            let body = CancelScansBody {
                provider,
                mode: None,
            };
            match client.cancel_scans().body(body).send().await {
                Ok(stopped) => {
                    let stopped = stopped.into_inner();
                    message.set(Some(i18n.args(
                        "console.scans.cancelled",
                        &[
                            ("runs", &stopped.runs.to_string()),
                            ("tasks", &stopped.tasks.to_string()),
                        ],
                    )));
                    tick.bump();
                }
                Err(e) => {
                    if !gate.refused(api::Refusal::of(&e)) {
                        message.set(Some(api::guarded_error(i18n, e)));
                    }
                }
            }
        });
    };

    let narrowed = filter.narrows_runs();
    rsx! {
        section { class: "ik-tile", style: "margin-bottom:18px;",
            div { class: "ik-flex", style: "justify-content:space-between;flex-wrap:wrap;",
                h3 { style: "margin:0;", {i18n.t("console.scans.title")} }
                div { class: "ik-flex",
                    select {
                        class: "ik-input",
                        style: "width:auto;",
                        "aria-label": i18n.t("console.scans.triggerMode"),
                        value: ScanModeExt::token(*mode.read()),
                        onchange: move |event: FormEvent| {
                            if let Some(chosen) = ScanModeExt::parse(&event.value()) {
                                mode.set(chosen);
                            }
                        },
                        for option in <ScanMode as ScanModeExt>::all().iter().copied() {
                            option {
                                key: "{ScanModeExt::token(option)}",
                                value: ScanModeExt::token(option),
                                {i18n.t(ScanModeExt::label_key(option))}
                            }
                        }
                    }
                    button { class: "ik-btn primary", onclick: trigger,
                        {i18n.t("console.scans.trigger")}
                    }
                    button { class: "ik-btn danger", onclick: drain,
                        {i18n.t("console.scans.cancelAll")}
                    }
                }
            }

            filters::FilterBar { filter: filter.clone() }

            if gate.is_open() {
                StepUpPrompt {
                    enrolled: true,
                    intro: Some(i18n.t("console.stepUp.intro")),
                    on_done: move |()| {
                        gate.close();
                        message.set(Some(i18n.t("stepUp.confirmedRetry")));
                    },
                }
            }
            if let Some(m) = message.read().clone() {
                p { class: "ik-muted", style: "margin:8px 0 0;", "{m}" }
            }

            // A failed summary must surface as an error rather than as zeroes: "nothing failed"
            // and "we could not ask" are the opposite conclusions on this screen.
            {
                async_block(
                    &summary,
                    tick.reload(),
                    120,
                    |fetched| {
                        let fetched = fetched.clone();
                        rsx! {
                            health::HealthStrip { summary: fetched, filter: filter.clone() }
                        }
                    },
                )
            }

            // One fetch drives both the live panel and the history table: a failed `list_scans`
            // must surface as an error, not render as "no runs" — indistinguishable from a quiet
            // system, the exact wrong conclusion here.
            {
                async_block(
                    &runs,
                    tick.reload(),
                    140,
                    |fetched| {
                        let merged = merge_runs(
                            &fetched.items,
                            live.runs.read().as_ref(),
                            narrowed,
                        );
                        // The stream wins once it has pushed: it is at most three seconds old,
                        // and the seed behind it is from whenever the panel opened.
                        let activity = live
                            .activity
                            .read()
                            .clone()
                            .or_else(|| activity_seed.read_unchecked().clone().flatten());
                        rsx! {
                            activity::LivePanel { runs: merged.clone(), activity, tick }
                            history::RunHistory {
                                runs: merged,
                                total: fetched.total,
                                narrowed,
                            }
                        }
                    },
                )
            }

            failures::FailuresSection {
                filter: filter.clone(),
                failures,
                groups,
                tick,
            }
        }
    }
}

/// A run's wall clock in seconds, measured to now while it is still in flight.
///
/// Reads the clock only for a run that has not finished: a finished run's duration is the
/// difference between its own two stamps, and using `now` there would make a completed run's
/// elapsed time keep growing on screen.
fn elapsed_seconds(run: &ScanRun) -> Option<f64> {
    const MS_PER_SECOND: f64 = 1_000.0;
    let started = run.started_at.as_deref().and_then(parse_ms)?;
    let ended = run
        .finished_at
        .as_deref()
        .and_then(parse_ms)
        .unwrap_or_else(crate::platform::now_ms);
    Some(((ended - started) / MS_PER_SECOND).max(0.0))
}

/// How long ago `stamp` was, in seconds, or `None` if it cannot be read.
fn age_seconds(stamp: Option<&str>) -> Option<f64> {
    const MS_PER_SECOND: f64 = 1_000.0;
    let then = stamp.and_then(parse_ms)?;
    Some(((crate::platform::now_ms() - then) / MS_PER_SECOND).max(0.0))
}

/// An RFC 3339 stamp as epoch milliseconds, or `None` if it cannot be parsed.
fn parse_ms(stamp: &str) -> Option<f64> {
    let ms = crate::platform::parse_timestamp_ms(stamp);
    ms.is_finite().then_some(ms)
}

/// A duration in seconds, worded — `41s`, `7m`, `2h 15m`.
fn duration_label(i18n: crate::i18n::Translator, seconds: f64) -> String {
    const SECONDS_PER_MINUTE: f64 = 60.0;
    const SECONDS_PER_HOUR: f64 = 3_600.0;
    if seconds < SECONDS_PER_MINUTE {
        return i18n.args("time.seconds", &[("count", &format!("{seconds:.0}"))]);
    }
    if seconds < SECONDS_PER_HOUR {
        let minutes = seconds / SECONDS_PER_MINUTE;
        return i18n.args("time.minutes", &[("count", &format!("{minutes:.0}"))]);
    }
    let hours = (seconds / SECONDS_PER_HOUR).floor();
    let minutes = ((seconds - hours * SECONDS_PER_HOUR) / SECONDS_PER_MINUTE).floor();
    i18n.args(
        "time.hoursMinutes",
        &[
            ("hours", &format!("{hours:.0}")),
            ("minutes", &format!("{minutes:.0}")),
        ],
    )
}

/// A ratio in `0.0..=1.0` as a whole percentage, clamped so a counter that has run ahead of its
/// total cannot render a bar wider than its track.
#[expect(
    clippy::cast_possible_truncation,
    reason = "the clamp makes the value a whole percentage before the cast"
)]
fn percent(ratio: f64) -> i32 {
    (ratio * 100.0).round().clamp(0.0, 100.0) as i32
}

/// The scope a run covers, worded: its provider's slug, or "all providers".
fn scope_label(i18n: crate::i18n::Translator, run: &ScanRun) -> String {
    run.provider_slug
        .clone()
        .unwrap_or_else(|| match run.provider_id {
            // A run scoped to a provider that has since been deleted: the id is all that is
            // left, and truncating it is better than claiming the run covered everything.
            Some(_) => i18n.t("console.scans.scopeUnknown"),
            None => i18n.t("console.scans.scopeAll"),
        })
}

#[cfg(test)]
mod tests {
    use super::{merge_runs, ScanFilter, STATE_FILTERS};
    use crate::models::{RunSort, RunSortExt, ScanMode, ScanModeExt, ScanRun};
    use crate::views::console::query::Window;
    use crate::views::console::ConsoleQuery;

    fn a_run(id: &str, done: i32) -> ScanRun {
        ScanRun {
            id: id.parse().expect("a run id"),
            provider_id: None,
            provider_slug: None,
            mode: ScanMode::Fast,
            state: crate::models::RunState::Running,
            total_tasks: 10,
            done_tasks: done,
            failed_tasks: 0,
            started_at: None,
            finished_at: None,
            created_at: "2026-08-09T00:00:00Z".to_owned(),
        }
    }

    /// The defect this panel was rebuilt around: with a filter applied, the live push used to be
    /// rendered *instead of* the filtered fetch, so choosing "failed" or a provider changed
    /// nothing on screen. The push may only refresh rows the fetch returned.
    #[test]
    fn a_filtered_list_never_shows_a_run_the_filter_excluded() {
        let kept = "018f4c2a-0000-7000-8000-000000000001";
        let excluded = "018f4c2a-0000-7000-8000-000000000002";
        let fetched = vec![a_run(kept, 1)];
        let pushed = vec![a_run(kept, 7), a_run(excluded, 3)];

        let merged = merge_runs(&fetched, Some(&pushed), true);
        assert_eq!(merged.len(), 1, "the pushed extra row is not in the filter");
        assert_eq!(
            merged[0].done_tasks, 7,
            "the kept row still takes the fresher counters"
        );
    }

    /// With nothing narrowing the list the push *is* the better answer — it is two seconds old
    /// and the fetch is from whenever the panel opened.
    #[test]
    fn an_unfiltered_list_shows_the_live_push_whole() {
        let fetched = vec![a_run("018f4c2a-0000-7000-8000-000000000001", 1)];
        let pushed = vec![
            a_run("018f4c2a-0000-7000-8000-000000000001", 7),
            a_run("018f4c2a-0000-7000-8000-000000000002", 3),
        ];
        assert_eq!(merge_runs(&fetched, Some(&pushed), false).len(), 2);
    }

    /// Before a single push has landed there is nothing to merge, and the fetch stands alone.
    #[test]
    fn without_a_push_the_fetch_stands() {
        let fetched = vec![a_run("018f4c2a-0000-7000-8000-000000000001", 1)];
        assert_eq!(merge_runs(&fetched, None, false).len(), 1);
        assert_eq!(merge_runs(&fetched, None, true).len(), 1);
    }

    /// Each control has to register as narrowing the list, or it silently loses to the push.
    #[test]
    fn every_control_narrows_the_run_list() {
        assert!(!ScanFilter::of(&ConsoleQuery::default()).narrows_runs());
        let cases = [
            ConsoleQuery {
                provider: Some("kunmanga".to_owned()),
                ..ConsoleQuery::default()
            },
            ConsoleQuery {
                status: Some("failed".to_owned()),
                ..ConsoleQuery::default()
            },
            ConsoleQuery {
                mode: Some("full".to_owned()),
                ..ConsoleQuery::default()
            },
            ConsoleQuery {
                sort: Some("failures".to_owned()),
                ..ConsoleQuery::default()
            },
            ConsoleQuery {
                since: Window::Day,
                ..ConsoleQuery::default()
            },
        ];
        for case in cases {
            assert!(
                ScanFilter::of(&case).narrows_runs(),
                "{case} does not narrow the run list"
            );
        }
    }

    /// A `?sort=` or `?mode=` an older link carries, or a hand-typed one, must open the panel
    /// rather than break it.
    #[test]
    fn an_unknown_filter_token_falls_back_instead_of_failing() {
        let query = ConsoleQuery {
            sort: Some("nonsense".to_owned()),
            mode: Some("nonsense".to_owned()),
            status: Some("nonsense".to_owned()),
            ..ConsoleQuery::default()
        };
        let filter = ScanFilter::of(&query);
        assert_eq!(filter.ordering(), RunSort::Recent);
        assert_eq!(filter.scan_mode(), None);
        assert_eq!(filter.run_state(), None);
    }

    /// The state picker's tokens have to survive the round trip through the URL and back into
    /// the wire enum, or a filter reads as "any state" after a reload.
    #[test]
    fn every_offered_run_state_parses_back() {
        for (token, state) in STATE_FILTERS {
            let query = ConsoleQuery {
                status: Some(token.to_owned()),
                ..ConsoleQuery::default()
            };
            assert_eq!(ScanFilter::of(&query).run_state(), Some(state));
        }
    }

    /// Every ordering and every mode the picker offers must be one the API accepts, read out of
    /// the committed document rather than trusted — `web/frontend` is a separate workspace, so
    /// no compiler relates these lists to the handler's enums.
    #[test]
    fn the_sort_picker_offers_every_published_ordering() {
        const SPEC: &str = include_str!("../../../../../../openapi.json");
        let spec: serde_json::Value = serde_json::from_str(SPEC).expect("openapi.json parses");
        let published = spec["paths"]["/v1/admin/scans"]["get"]["parameters"]
            .as_array()
            .expect("the run list declares parameters")
            .iter()
            .find(|param| param["name"] == "sort")
            .expect("the run list publishes a sort parameter")["schema"]["enum"]
            .as_array()
            .expect("the sort parameter is an enumeration")
            .iter()
            .map(|value| value.as_str().unwrap_or_default().to_owned())
            .collect::<Vec<_>>();
        let offered = <RunSort as RunSortExt>::all()
            .iter()
            .map(|sort| sort.token().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(offered, published);
    }

    /// Wording is a separate axis from membership, and a missing catalogue key renders as the
    /// key itself rather than as an error — so an unworded option ships `console.scan.sort.…`
    /// to the operator.
    #[test]
    fn every_offered_ordering_and_mode_is_worded() {
        for sort in <RunSort as RunSortExt>::all().iter().copied() {
            assert!(crate::i18n::has_key(sort.label_key()), "{sort:?}");
        }
        for mode in <ScanMode as ScanModeExt>::all().iter().copied() {
            assert!(crate::i18n::has_key(mode.label_key()), "{mode:?}");
        }
    }
}
