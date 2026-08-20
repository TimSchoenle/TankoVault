//! The canonicalisation review queue: merge and dismiss actions, the standing duplicate sweep,
//! and the compact read-only series card the compare view is built from.
//!
//! Merge direction defaults to the server's `suggested_keep`, swappable by the operator, because
//! merging into the wrong side discards the richer series — the absorbed id stops existing and
//! everything pointing at it breaks.

use crate::api;
use crate::components::{
    async_block, async_block_list, async_view, use_step_up_gate, Cover, SkeletonBlock, StepUpGate,
    StepUpGuard,
};
use crate::hooks::{use_reload, Reload};
use crate::i18n::{use_i18n, Translator};
use crate::models::*;
use crate::state::capabilities::use_capabilities;
use crate::state::use_session;
use crate::views::console::query::Band;
use crate::views::console::tuning::{Knob, KnobRow};
use crate::views::console::{use_console_nav, ConsoleQuery};
use crate::wire::types::{MergeFullSweepStatusView, Permission, TunableGroup};
use dioxus::prelude::*;
use inkstone_ui::{Button, Pill, ToggleButton, Tone};
use progenitor_client::ResponseValue;

/// How often the exhaustive sweep's state is re-read while a run holds the claim.
///
/// Fast enough that the counters visibly move, slow enough to be nothing beside the work the run
/// is doing. Only ticks while a run is live; see [`FullSweep`].
const FULL_SWEEP_POLL_MS: u32 = 3_000;

/// The tuning groups this panel renders, each with the catalogue key that titles it.
///
/// One entry, and a list anyway: `every_published_group_is_offered_and_worded` reads it against
/// the vocabulary the API publishes, so a group added to the registry has to land on one of the
/// console's panels rather than silently on neither.
pub(in crate::views::console) const POLICY_GROUPS: [(TunableGroup, &str); 1] =
    [(TunableGroup::Matching, "console.merge.policy.title")];

/// Canonicalisation review queue with merge / dismiss actions and the duplicate sweep.
#[component]
pub(super) fn MergeQueue() -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let session = use_session();
    let reload = use_reload();
    let nav = use_console_nav();
    let band = nav.query().band;
    let mut notice = use_signal(String::new);
    let mut busy = use_signal(|| false);
    // One gate for the whole queue, handed down to the rows: the elevation it earns covers
    // every action on the panel, and a prompt per row would put the same question on screen
    // once for each candidate.
    let gate = use_step_up_gate();

    let resource = use_resource(move || {
        reload.track();
        let threshold = band.min_score();
        let client = api.client();
        async move {
            client
                .list_merge_candidates()
                .min_score(threshold)
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });

    let empty = i18n.t("console.merge.empty");
    let body = async_block_list(&resource, reload, 60, &empty, |list| {
        let list = list.to_vec();
        rsx! {
            for c in list {
                MergeRow { key: "{c.id}", candidate: Signal::new(c), reload, gate }
            }
        }
    });

    // Sweep acts on rows the operator can't see, so its outcome is reported as text, not
    // inferred from the list changing length.
    let run_sweep = move |_| {
        gate.attempt(move || {
            if *busy.peek() {
                return;
            }
            busy.set(true);
            notice.set(String::new());
            spawn(async move {
                let client = gate.client(api);
                if session.token_value().is_some() {
                    match client.sweep_merge_candidates().send().await {
                        Ok(r) => {
                            let r = r.into_inner();
                            notice.set(i18n.args(
                                "console.merge.sweepDone",
                                &[
                                    ("examined", &r.pairs_examined.to_string()),
                                    ("merged", &r.auto_merged.to_string()),
                                    // Newly queued and reopened both lengthen the queue; rescored
                                    // ones do not. Reporting them as one number told an operator
                                    // the queue had grown by the count of rows it merely re-read.
                                    ("queued", &(r.queued + r.reopened).to_string()),
                                    ("rescored", &r.requeued.to_string()),
                                    ("withdrawn", &r.withdrawn.to_string()),
                                ],
                            ));
                            reload.bump();
                        }
                        Err(e) => {
                            if !gate.refused(api::Refusal::of(&e)) {
                                notice.set(i18n.args(
                                    "console.merge.actionFailed",
                                    &[("message", &api::guarded_error(i18n, e))],
                                ));
                            }
                        }
                    }
                }
                busy.set(false);
            });
        });
    };

    let rebuild_keys = move |_| {
        gate.attempt(move || {
            if *busy.peek() {
                return;
            }
            busy.set(true);
            notice.set(String::new());
            spawn(async move {
                let client = gate.client(api);
                if session.token_value().is_some() {
                    match client.rebuild_matching_keys().send().await {
                        Ok(r) => {
                            let r = r.into_inner();
                            notice.set(i18n.args(
                                "console.merge.rebuildDone",
                                &[
                                    ("series", &r.series_updated.to_string()),
                                    ("titles", &r.titles_updated.to_string()),
                                ],
                            ));
                            reload.bump();
                        }
                        Err(e) => {
                            if !gate.refused(api::Refusal::of(&e)) {
                                notice.set(i18n.args(
                                    "console.merge.actionFailed",
                                    &[("message", &api::guarded_error(i18n, e))],
                                ));
                            }
                        }
                    }
                }
                busy.set(false);
            });
        });
    };

    rsx! {
        section {
            h3 { {i18n.t("console.tab.merge")} }
            div { class: "ik-row", style: "gap:8px;flex-wrap:wrap;margin-bottom:12px;",
                div { class: "ik-flex", style: "gap:4px;flex-wrap:wrap;",
                    for option in Band::ALL {
                        ToggleButton {
                            key: "{option.token()}",
                            on: option == band,
                            on_toggle: move |_| nav.filter(ConsoleQuery { band: option, ..nav.query() }),
                            {i18n.t(option.label_key())}
                        }
                    }
                }
                div { class: "grow" }
                Button {
                    disabled: *busy.read(),
                    on_click: run_sweep,
                    {i18n.t("console.merge.sweep")}
                }
                Button {
                    disabled: *busy.read(),
                    on_click: rebuild_keys,
                    {i18n.t("console.merge.rebuildKeys")}
                }
            }
            FullSweep { reload, gate }
            StepUpGuard { gate, intro: Some(i18n.t("console.stepUp.intro")) }
            if !notice.read().is_empty() {
                div { class: "ik-card", style: "margin-bottom:12px;padding:10px;",
                    "{notice}"
                }
            }
            MergePolicy { reload, gate }
            {body}
        }
    }
}

/// The exhaustive sweep: the button that starts one, and the state of the run it started.
///
/// It has a surface of its own because it outlives the request that starts it. The button beside
/// it reports itself in the response body — one sweep, one report — but an exhaustive run is
/// detached and takes minutes, so the only honest place for its outcome is something that keeps
/// re-reading it. Without that, the operator who pressed the button has no way to tell a run
/// that is working from one that has stopped.
#[component]
fn FullSweep(reload: Reload, gate: StepUpGate) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let session = use_session();
    let tick = use_reload();
    let mut busy = use_signal(|| false);
    let mut notice = use_signal(String::new);

    let status = use_resource(move || {
        reload.track();
        tick.track();
        let client = api.client();
        async move {
            client
                .full_merge_sweep_status()
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });

    // Only while a run holds the claim. A finished run's figures do not move, so a standing timer
    // over them would be a request every few seconds for an answer that cannot change. The flag
    // is read *inside* the loop rather than captured from the render that spawned it: this future
    // runs once, and a copy taken at mount would be `false` for a run started from this button.
    use_future(move || async move {
        loop {
            crate::platform::sleep_ms(FULL_SWEEP_POLL_MS).await;
            if matches!(&*status.read_unchecked(), Some(Ok(view)) if view.running) {
                tick.bump();
            }
        }
    });

    let start = move |_| {
        gate.attempt(move || {
            if *busy.peek() {
                return;
            }
            busy.set(true);
            notice.set(String::new());
            spawn(async move {
                let client = gate.client(api);
                if session.token_value().is_some() {
                    match client.sweep_all_merge_candidates().send().await {
                        Ok(r) => {
                            notice.set(i18n.t(if r.into_inner().started {
                                "console.merge.fullSweepStarted"
                            } else {
                                // A run already held the claim. Not an error: the run in flight
                                // is doing this one's work, and the line below tracks it.
                                "console.merge.fullSweepBusy"
                            }));
                            // The poll reads the *fetched* state, which still says idle until
                            // this refetch lands — without it nothing would ever start ticking.
                            tick.bump();
                        }
                        Err(e) => {
                            if !gate.refused(api::Refusal::of(&e)) {
                                notice.set(i18n.args(
                                    "console.merge.actionFailed",
                                    &[("message", &api::guarded_error(i18n, e))],
                                ));
                            }
                        }
                    }
                }
                busy.set(false);
            });
        });
    };

    let (line, running) = match &*status.read() {
        Some(Ok(view)) => (full_sweep_line(i18n, view), view.running),
        _ => (None, false),
    };

    rsx! {
        div { class: "ik-card", style: "margin-bottom:12px;padding:10px;",
            div { class: "ik-flex", style: "gap:8px;align-items:center;flex-wrap:wrap;",
                Button {
                    disabled: running || *busy.read(),
                    on_click: start,
                    {i18n.t("console.merge.fullSweep")}
                }
                span { class: "ik-muted", style: "font-size:12px;max-width:74ch;",
                    {line.unwrap_or_else(|| i18n.t("console.merge.fullSweepIdle"))}
                }
            }
            if !notice.read().is_empty() {
                p { style: "font-size:12px;margin:8px 0 0;", "{notice}" }
            }
        }
    }
}

/// The one-line account of the exhaustive sweep: what it is doing, or how the last run ended.
///
/// `None` before any run has been recorded, so the panel can say "never run" rather than render
/// a row of zeroes that reads like a run which found nothing.
fn full_sweep_line(i18n: Translator, view: &MergeFullSweepStatusView) -> Option<String> {
    let rounds = view.rounds.to_string();
    let examined = view.pairs_examined.to_string();
    let merged = view.auto_merged.to_string();
    // Newly queued and reopened both lengthen the queue; re-scored rows do not — the same reading
    // as the single sweep's report, so the two sentences are counting the same thing.
    let queued = (view.queued + view.reopened).to_string();
    let withdrawn = view.withdrawn.to_string();
    let counts = [
        ("rounds", rounds.as_str()),
        ("examined", examined.as_str()),
        ("merged", merged.as_str()),
        ("queued", queued.as_str()),
        ("withdrawn", withdrawn.as_str()),
    ];

    if view.running {
        return Some(i18n.args("console.merge.fullSweepRunning", &counts));
    }
    match view.stopped.as_deref()? {
        "exhausted" => Some(i18n.args("console.merge.fullSweepDone", &counts)),
        "merge_ceiling" => Some(i18n.args("console.merge.fullSweepCeiling", &counts)),
        "round_cap" => Some(i18n.args("console.merge.fullSweepCapped", &counts)),
        // `failed`, and any reason a later control plane adds: say the run stopped and show what
        // it said, rather than inventing a sentence for a reason this build does not know.
        _ => Some(i18n.args(
            "console.merge.fullSweepFailed",
            &[("message", view.error.as_deref().unwrap_or("-"))],
        )),
    }
}

/// The policy the sweep applies, above the queue it produced.
///
/// Here rather than on a page of its own because the two are one decision: an operator reads a
/// row the sweep queued — or one it merged — and the question that follows is whether the bar
/// that put it there is the right bar. A separate page would mean judging the queue and tuning
/// the rule that fills it without ever seeing them together.
#[component]
fn MergePolicy(reload: Reload, gate: StepUpGate) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let caps = use_capabilities();
    let can_write = caps.can(Permission::MergeWrite);
    let mut open = use_signal(|| false);

    // Fetched only once the card is open, so the queue — the reason an operator is on this tab —
    // does not pay for a panel most visits never expand.
    let policy = use_resource(move || {
        reload.track();
        let wanted = *open.read();
        let client = api.client();
        async move {
            if !wanted {
                return Ok(Vec::new());
            }
            client
                .get_merge_policy()
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });

    // Collapsed by default: the queue is what an operator came for, and the policy is what they
    // open once they disagree with it.
    rsx! {
        div { class: "ik-card", style: "margin-bottom:12px;padding:10px;",
            div { class: "ik-flex", style: "gap:8px;align-items:center;",
                strong { style: "font-size:13px;", {i18n.t(POLICY_GROUPS[0].1)} }
                div { class: "grow" }
                Button {
                    on_click: move |_| { let v = *open.peek(); open.set(!v); },
                    if *open.read() {
                        {i18n.t("console.merge.policy.hide")}
                    } else {
                        {i18n.t("console.merge.policy.show")}
                    }
                }
            }
            if *open.read() {
                p { class: "ik-muted", style: "font-size:12px;margin:8px 0 0;max-width:74ch;",
                    {i18n.t("console.merge.policy.intro")}
                }
                if !can_write {
                    p { class: "ik-muted", style: "font-size:12px;margin:4px 0 0;",
                        {i18n.t("console.merge.policy.readOnly")}
                    }
                }
                {
                    async_view(
                        &policy,
                        reload,
                        || rsx! { SkeletonBlock { height: 260 } },
                        |rows| rsx! {
                            div { class: "ik-tablewrap", style: "margin-top:10px;",
                                for row in rows.iter().map(Knob::matching) {
                                    KnobRow { key: "{row.key}", row, can_write, reload, gate }
                                }
                            }
                        },
                    )
                }
            }
        }
    }
}

#[component]
pub(super) fn MergeRow(
    candidate: Signal<MergeCandidate>,
    reload: Reload,
    /// The queue's gate, so a refusal opens the one prompt above the list rather than a
    /// duplicate inside every row.
    gate: StepUpGate,
) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let session = use_session();
    let can = candidate.read();
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the score is a 0..=1 ratio and the clamp makes the percentage total rather \
                  than trusting the input to be well-formed"
    )]
    let pct = (can.score * 100.0).round().clamp(0.0, 100.0) as i32;
    let id = can.id;
    let mut open = use_signal(|| false);
    let mut busy = use_signal(|| false);
    // Which side survives; seeded from the server's suggestion, swappable since the operator
    // can see something the counts can't.
    let mut keep_first = use_signal(|| keeps_first_by_default(&can));

    let score_class = if pct >= 90 {
        "ik-mono acc"
    } else if pct >= 75 {
        "ik-mono jade"
    } else {
        "ik-mono ik-muted"
    };

    let signals = can.signals.clone();
    let reason = can.reason.clone();
    let sides = MergeSides::of(&can, *keep_first.read());
    drop(can);

    let MergeSides {
        keep,
        drop: drop_side,
    } = sides;
    let keep_id = keep.id;
    let drop_id = drop_side.id;

    let merge = move |_| {
        gate.attempt(move || {
            if *busy.peek() {
                return;
            }
            busy.set(true);
            spawn(async move {
                let client = gate.client(api);
                if session.token_value().is_some() {
                    match client
                        .merge_series()
                        .body(MergeRequest {
                            keep: keep_id,
                            merge: drop_id,
                        })
                        .send()
                        .await
                    {
                        Ok(_) => reload.bump(),
                        // The row has no error line of its own, so anything but a step-up demand
                        // still leaves the queue as it was — but the demand has to reach the
                        // panel's prompt, or the button silently does nothing forever.
                        Err(e) => {
                            let _refused = gate.refused(api::Refusal::of(&e));
                        }
                    }
                }
                busy.set(false);
            });
        });
    };

    let dismiss = move |_| {
        gate.attempt(move || {
            if *busy.peek() {
                return;
            }
            busy.set(true);
            spawn(async move {
                let client = gate.client(api);
                if session.token_value().is_some() {
                    match client
                        .dismiss_merge_candidate()
                        .body(DismissRequest { id })
                        .send()
                        .await
                    {
                        Ok(_) => reload.bump(),
                        // See the merge above: the demand reaches the panel's prompt, everything
                        // else leaves the row alone.
                        Err(e) => {
                            let _refused = gate.refused(api::Refusal::of(&e));
                        }
                    }
                }
                busy.set(false);
            });
        });
    };

    let keep_title = keep.title.clone();
    rsx! {
        div { class: "ik-card", style: "margin-bottom:10px;",
            div { class: "ik-row",
                div { class: "grow",
                    div { class: "ik-flex", style: "justify-content:space-between;align-items:center;",
                        span { style: "font-weight:600;", "{keep.title}" }
                        span { class: "{score_class}",
                            {i18n.args("console.merge.score", &[("percent", &pct.to_string())])}
                        }
                    }
                    div { class: "ik-muted", style: "font-size:13px;", "↔ {drop_side.title}" }
                    div { class: "ik-flex", style: "gap:4px;margin-top:6px;flex-wrap:wrap;",
                        for s in signals.iter() {
                            Pill {
                                key: "{s}",
                                style: "font-size:11px;",
                                {crate::views::console::signal_label(i18n, s)}
                            }
                        }
                    }
                    div { class: "ik-muted", style: "font-size:12px;margin-top:4px;",
                        {
                            i18n.args(
                                "console.merge.sides",
                                &[
                                    ("keep", &keep.summary(i18n)),
                                    ("drop", &drop_side.summary(i18n)),
                                ],
                            )
                        }
                    }
                    if let Some(r) = &reason {
                        div { class: "ik-muted", style: "font-size:12px;",
                            {i18n.args("console.merge.reason", &[("reason", r)])}
                        }
                    }
                }
                Button {
                    on_click: move |_| { let v = *keep_first.peek(); keep_first.set(!v); },
                    {i18n.t("console.merge.swap")}
                }
                Button {
                    on_click: move |_| { let v = *open.peek(); open.set(!v); },
                    if *open.read() {
                    {i18n.t("console.merge.hide")}
                    } else {
                    {i18n.t("console.merge.compare")}
                    }
                }
                Button {
                    tone: Tone::Primary,
                    disabled: *busy.read(),
                    title: "{keep_title}",
                    on_click: merge,
                    {i18n.t("console.merge.merge")}
                }
                Button {
                    disabled: *busy.read(),
                    on_click: dismiss,
                    {i18n.t("console.merge.distinct")}
                }
            }
            if *open.read() {
                div { class: "ik-flex", style: "gap:14px;margin-top:12px;align-items:stretch;flex-wrap:wrap;",
                    div { style: "flex:1;min-width:240px;",
                        div { class: "ik-pill jade", style: "margin-bottom:6px;",
                            {i18n.t("console.merge.keep")}
                        }
                        SeriesMiniCard { series_id: keep_id }
                    }
                    div { style: "flex:1;min-width:240px;",
                        div { class: "ik-pill", style: "margin-bottom:6px;",
                            {i18n.t("console.merge.drop")}
                        }
                        SeriesMiniCard { series_id: drop_id }
                    }
                }
            }
        }
    }
}

/// One side of a candidate pair, as the row header summarises it.
#[derive(Clone, PartialEq)]
struct SideSummary {
    id: SeriesId,
    title: String,
    sources: i64,
    chapters: i64,
}

impl SideSummary {
    /// "3 sources · 412 chapters" — the two numbers that decide which side should survive.
    fn summary(&self, i18n: crate::i18n::Translator) -> String {
        let sources = i18n.plural("series.sources", self.sources, &[]);
        let chapters = i18n.args(
            "series.chapterCount",
            &[("count", &self.chapters.to_string())],
        );
        format!("{sources} · {chapters}")
    }
}

/// Whether the server's `suggested_keep` names the pair's first side.
///
/// Seeds the swap toggle, and is therefore what decides the direction of every merge the
/// operator does not explicitly swap. Inverting it discards the side the scorer judged richer,
/// and the absorbed id stops existing — so nothing downstream can report the mistake.
fn keeps_first_by_default(candidate: &MergeCandidate) -> bool {
    candidate.suggested_keep == candidate.series_id
}

/// A candidate pair oriented into the merge it would perform.
#[derive(Clone, PartialEq)]
struct MergeSides {
    /// The series that survives and absorbs the other.
    keep: SideSummary,
    /// The series that stops existing.
    drop: SideSummary,
}

impl MergeSides {
    /// Orient a candidate. `keep_first` is [`keeps_first_by_default`] as the operator has
    /// possibly since flipped it.
    fn of(candidate: &MergeCandidate, keep_first: bool) -> Self {
        let first = SideSummary {
            id: candidate.series_id,
            title: candidate.series_title.clone(),
            sources: candidate.series_sources,
            chapters: candidate.series_chapters,
        };
        let second = SideSummary {
            id: candidate.candidate_id,
            title: candidate.candidate_title.clone(),
            sources: candidate.candidate_sources,
            chapters: candidate.candidate_chapters,
        };
        if keep_first {
            Self {
                keep: first,
                drop: second,
            }
        } else {
            Self {
                keep: second,
                drop: first,
            }
        }
    }
}

/// Compact read-only "manga info" card for a single series, used by the merge compare view
/// and the Sync inspector. Fetches the public series detail so operators can eyeball cover,
/// type/status, sources and tags before acting.
#[component]
pub(super) fn SeriesMiniCard(series_id: SeriesId) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let reload = use_reload();
    let res = use_resource(move || {
        reload.track();
        let client = api.client();
        async move {
            client
                .detail()
                .id(series_id)
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });

    async_block(&res, reload, 120, |d| {
        let d = d.clone();
        let year = d.release_year.map(|y| y.to_string()).unwrap_or_default();
        let tags: Vec<String> = d.tags.iter().take(6).map(|t| t.name.clone()).collect();
        rsx! {
            div { class: "ik-card", style: "padding:10px;",
                div { class: "ik-flex", style: "gap:10px;align-items:flex-start;",
                    div { style: "width:56px;flex:0 0 auto;",
                        Cover { url: d.cover_url.clone(), title: d.title.clone() }
                    }
                    div { class: "grow",
                        div { style: "font-weight:600;", "{d.title}" }
                        div { class: "ik-flex", style: "gap:6px;margin-top:4px;flex-wrap:wrap;",
                            Pill {
                                {i18n.t(d.content_type.label_key())}
                            }
                            Pill {
                                {i18n.t(d.status.label_key())}
                            }
                            if !year.is_empty() {
                                Pill {
                                    "{year}"
                                }
                            }
                            Pill {
                                {
                                i18n.plural(
                                "series.sources",
                                i64::try_from(d.sources.len()).unwrap_or(0),
                                &[],
                                )
                                }
                            }
                        }
                        div { class: "ik-mono ik-muted", style: "font-size:11px;margin-top:4px;word-break:break-all;",
                            "{d.id}"
                        }
                    }
                }
                if !d.sources.is_empty() {
                    div { style: "margin-top:8px;",
                        for s in d.sources.iter().take(5) {
                            div { class: "ik-muted", style: "font-size:12px;",
                                {
                                    let count = i18n.args(
                                        "series.chapterCount",
                                        &[("count", &s.chapter_count.to_string())],
                                    );
                                    format!("· {} — {count}", s.provider_name)
                                }
                            }
                        }
                    }
                }
                if !tags.is_empty() {
                    div { class: "ik-flex", style: "gap:4px;margin-top:8px;flex-wrap:wrap;",
                        for t in tags {
                            Pill {
                                style: "font-size:11px;",
                                "{t}"
                            }
                        }
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(suggested: SeriesId, first: SeriesId, second: SeriesId) -> MergeCandidate {
        MergeCandidate {
            candidate_chapters: 140,
            candidate_id: second,
            candidate_sources: 1,
            candidate_title: "Bell of the Ninth".to_owned(),
            created_at: "2026-07-29T00:00:00Z".to_owned(),
            id: uuid::Uuid::from_u128(9),
            reason: None,
            score: 0.91,
            series_chapters: 412,
            series_id: first,
            series_sources: 3,
            series_title: "Ninth Bell".to_owned(),
            signals: Vec::new(),
            suggested_keep: suggested,
            updated_at: "2026-07-29T00:00:00Z".to_owned(),
        }
    }

    /// The side the server suggests keeping is the side that survives an unswapped merge.
    ///
    /// This is the whole safety of the row: a merge discards the absorbed id outright, so an
    /// inverted default destroys the richer series on a click that looks routine, and there is
    /// nothing left afterwards for any check to notice.
    #[test]
    fn the_servers_suggestion_is_the_side_that_survives_by_default() {
        let (first, second) = (
            SeriesId(uuid::Uuid::from_u128(1)),
            SeriesId(uuid::Uuid::from_u128(2)),
        );

        let keeps_first = candidate(first, first, second);
        let sides = MergeSides::of(&keeps_first, keeps_first_by_default(&keeps_first));
        assert_eq!(sides.keep.id, first);
        assert_eq!(sides.drop.id, second);

        let keeps_second = candidate(second, first, second);
        let sides = MergeSides::of(&keeps_second, keeps_first_by_default(&keeps_second));
        assert_eq!(sides.keep.id, second);
        assert_eq!(sides.drop.id, first);
    }

    /// Swapping exchanges the two sides and nothing else — the counts and titles must travel
    /// with their own id, or the operator reads one series' figures while merging away another.
    #[test]
    fn swapping_exchanges_the_sides_with_their_own_figures() {
        let (first, second) = (
            SeriesId(uuid::Uuid::from_u128(1)),
            SeriesId(uuid::Uuid::from_u128(2)),
        );
        let row = candidate(first, first, second);

        let kept = MergeSides::of(&row, true);
        assert_eq!((kept.keep.chapters, kept.keep.sources), (412, 3));
        assert_eq!(kept.keep.title, "Ninth Bell");

        let swapped = MergeSides::of(&row, false);
        assert_eq!((swapped.keep.chapters, swapped.keep.sources), (140, 1));
        assert_eq!(swapped.keep.title, "Bell of the Ninth");
        assert_eq!(swapped.drop.id, kept.keep.id);
        assert_eq!(swapped.keep.id, kept.drop.id);
    }
}
