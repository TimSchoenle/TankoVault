//! The recommender's control plane: model health, the rebuild that makes a stored-model change
//! take effect, and the tuning registry (`docs/RECOMMENDATIONS.md` §8, §10).
//!
//! Deliberately not on the shared auto-refresh tick, for the reason `flags` is not: a background
//! refetch landing between an operator typing a value and pressing Save would discard it.
//!
//! Every row's wording — its title, its description, its bounds — comes from the server's
//! compiled registry rather than the catalogue here. A knob's meaning is defined where the
//! pipeline reads it, and a second copy in a locale file would drift the moment either moved.

use crate::api;
use crate::components::{
    async_view, use_step_up_gate, ErrorLine, Kpi, OutcomeLine, SkeletonBlock, StepUpGate,
    StepUpGuard,
};
use crate::hooks::{use_busy, use_outcome, use_reload, Busy, Outcome, Reload};
use crate::i18n::use_i18n;
use crate::icons::{Ic, Icon};
use crate::state::capabilities::use_capabilities;
use crate::util::{iso_date, rel_time, thousands};
use crate::wire::types::{
    Applies, ModelHealthView, Permission, RebuildRequest, RecsysBuildMode, SetTunable,
    TunableGroup, TunableKind, TunableView,
};
use dioxus::prelude::*;
use inkstone_ui::{Button, Pill, Tone};
use progenitor_client::ResponseValue;
/// How often the health panel re-reads itself while a build holds the claim.
///
/// Fast enough that a progress bar visibly moves, slow enough that it is nothing beside the work
/// the build is doing. Only ticks while `building`; see [`ModelHealth`].
const BUILD_POLL_MS: u32 = 3_000;

/// The build stages a full run passes through, in order, so the console can say "4 of 6" rather
/// than printing a bare token whose position in the sequence only the builder knows.
///
/// Hand-listed against `services/control-plane/src/recsys.rs`; a stage this does not know is
/// shown by name with no ordinal, which is the honest degradation — a wrong ordinal would claim
/// a run is further along than it is.
const FULL_STAGES: [&str; 6] = [
    "full:features",
    "full:vocabulary",
    "full:basis",
    "full:embedding",
    "full:index",
    "full:priors",
];

/// The groups in display order, each with the catalogue key that titles it.
///
/// Hand-listed because the generated client carries no way to enumerate a schema enum, and kept
/// honest by `every_published_group_is_offered_and_worded`.
const GROUPS: [(TunableGroup, &str); 8] = [
    (TunableGroup::Affinity, "console.recsys.group.affinity"),
    (TunableGroup::Retrieval, "console.recsys.group.retrieval"),
    (TunableGroup::Scoring, "console.recsys.group.scoring"),
    (TunableGroup::Diversity, "console.recsys.group.diversity"),
    (TunableGroup::Prior, "console.recsys.group.prior"),
    (TunableGroup::Build, "console.recsys.group.build"),
    (
        TunableGroup::Cooccurrence,
        "console.recsys.group.cooccurrence",
    ),
    (TunableGroup::Serving, "console.recsys.group.serving"),
];

#[component]
pub(super) fn RecommendationsPanel() -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let caps = use_capabilities();
    let reload = use_reload();
    let can_write = caps.can(Permission::RecsysWrite);
    // One gate for the panel: the rebuild and every knob below it are the same capability, so
    // an operator confirms once and the prompt has a single place to appear.
    let gate = use_step_up_gate();

    let tunables = use_resource(move || {
        reload.track();
        let client = api.client();
        async move {
            client
                .list_tunables()
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });

    rsx! {
        section { style: "margin-bottom:18px;",
            h3 { {i18n.t("console.tab.recommendations")} }
            p { class: "ik-muted", style: "font-size:13px;margin-top:0;max-width:70ch;",
                {i18n.t("console.recsys.intro")}
            }
            if !can_write {
                p { class: "ik-muted", style: "font-size:12px;", {i18n.t("console.recsys.readOnly")} }
            }
            StepUpGuard { gate, intro: Some(i18n.t("console.stepUp.intro")) }

            ModelHealth { can_write, reload, gate }

            div { class: "ik-subhead", style: "margin-top:22px;", {i18n.t("console.recsys.tuning")} }
            {
                async_view(
                    &tunables,
                    reload,
                    || rsx! { SkeletonBlock { height: 420 } },
                    |rows| rsx! {
                        for (group , title_key) in GROUPS {
                            TunableGroupSection {
                                key: "{title_key}",
                                title: i18n.t(title_key),
                                rows: rows.iter().filter(|row| row.group == group).cloned().collect::<Vec<_>>(),
                                can_write,
                                reload,
                                gate,
                            }
                        }
                    },
                )
            }
        }
    }
}

/// What the model is, how much of the catalogue it covers, and the two rebuilds.
///
/// This one panel *is* on a refresh tick, unlike the tuning rows below it — and for the same
/// reason they are not. A background refetch is destructive over a half-typed value; over a
/// read-only progress figure it is the entire point. A build takes minutes and the operator who
/// pressed the button has no other way to tell a run that is working from one that has hung,
/// short of reloading the page repeatedly.
#[component]
fn ModelHealth(can_write: bool, reload: Reload, gate: StepUpGate) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let tick = use_reload();
    let health = use_resource(move || {
        reload.track();
        tick.track();
        let client = api.client();
        async move {
            client
                .model_health()
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });

    // Only while something is running. An idle model's figures do not move, so a standing timer
    // over them would be a request every few seconds for an answer that cannot change.
    let building = matches!(&*health.read_unchecked(), Some(Ok(view)) if view.building);
    use_future(move || async move {
        loop {
            crate::platform::sleep_ms(BUILD_POLL_MS).await;
            if building {
                tick.bump();
            }
        }
    });

    rsx! {
        {
            async_view(
                &health,
                reload,
                || rsx! { SkeletonBlock { height: 180 } },
                |view| rsx! { HealthBody { view: view.clone(), can_write, reload, gate } },
            )
        }
    }
}

/// The health figures themselves, once loaded.
#[component]
fn HealthBody(view: ModelHealthView, can_write: bool, reload: Reload, gate: StepUpGate) -> Element {
    let i18n = use_i18n();

    // The gap between what has an embedding and what may be recommended is the figure that
    // explains a thin shelf, so it is stated rather than left to be subtracted by eye.
    let excluded = view.series_with_embedding - view.series_recommendable;
    let repairing = view.repair_queue_depth > 0;
    let coverage_accent = if excluded > 0 { "warn" } else { "" }.to_owned();
    let repair_accent = if repairing { "warn" } else { "" }.to_owned();

    rsx! {
        div { class: "ik-flex", style: "gap:8px;align-items:center;margin-bottom:12px;flex-wrap:wrap;",
            span {
                class: if view.building { "ik-pill run" } else { "ik-pill" },
                "{view.stage}"
            }
            span { class: "ik-mono ik-muted", style: "font-size:11.5px;",
                {i18n.args("console.recsys.generation", &[("n", &view.generation.to_string())])}
            }
            if let Some(finished) = view.finished_at.clone() {
                span { class: "ik-mono ik-muted", style: "font-size:11.5px;",
                    {
                        let when = iso_date(Some(finished.as_str())).to_owned();
                        i18n.args("console.recsys.lastBuilt", &[("date", &when)])
                    }
                }
            } else if let Some(started) = view.started_at.clone() {
                span { class: "ik-mono ik-muted", style: "font-size:11.5px;",
                    {
                        let when = iso_date(Some(started.as_str())).to_owned();
                        i18n.args("console.recsys.startedAt", &[("date", &when)])
                    }
                }
            }
            if can_write {
                div { class: "ik-flex", style: "gap:6px;margin-left:auto;",
                    RebuildButton {
                        mode: RecsysBuildMode::Incremental,
                        label: i18n.t("console.recsys.rebuildIncremental"),
                        hint: i18n.t("console.recsys.rebuildIncrementalHint"),
                        building: view.building,
                        reload,
                        gate,
                    }
                    RebuildButton {
                        mode: RecsysBuildMode::Full,
                        label: i18n.t("console.recsys.rebuildFull"),
                        hint: i18n.t("console.recsys.rebuildFullHint"),
                        building: view.building,
                        reload,
                        gate,
                    }
                }
            }
        }

        if view.building {
            BuildProgress {
                stage: view.stage.clone(),
                done: view.series_built,
                total: view.stage_total,
                started_at: view.started_at.clone(),
            }
        }

        // A failed run releases its claim, so this is the only place the failure is visible at
        // all — an unbuilt model and a broken one look identical in the coverage figures.
        if let Some(error) = view.error.clone() {
            p { style: "font-size:12.5px;color:var(--acc);margin:0 0 12px;max-width:74ch;",
                {i18n.args("console.recsys.lastError", &[("message", &error)])}
            }
        }

        div { class: "ik-kpis",
            Kpi {
                label: i18n.t("console.recsys.kpi.recommendable"),
                value: thousands(view.series_recommendable),
                sub: i18n.args(
                    "console.recsys.kpi.recommendableSub",
                    &[("excluded", &thousands(excluded))],
                ),
                accent: coverage_accent,
            }
            Kpi {
                label: i18n.t("console.recsys.kpi.embedded"),
                value: thousands(view.series_with_embedding),
                sub: i18n.args(
                    "console.recsys.kpi.embeddedSub",
                    &[("features", &thousands(view.series_with_features))],
                ),
            }
            Kpi {
                label: i18n.t("console.recsys.kpi.catalogue"),
                value: thousands(view.series_total),
                sub: i18n.t("console.recsys.kpi.catalogueSub"),
            }
            Kpi {
                label: i18n.t("console.recsys.kpi.vocabulary"),
                value: thousands(i64::from(view.vocabulary)),
                sub: i18n.args(
                    "console.recsys.kpi.vocabularySub",
                    &[("dims", &view.dense_dims.to_string())],
                ),
            }
            Kpi {
                label: i18n.t("console.recsys.kpi.repairQueue"),
                value: thousands(view.repair_queue_depth),
                sub: i18n.t("console.recsys.kpi.repairQueueSub"),
                accent: repair_accent,
            }
            Kpi {
                label: i18n.t("console.recsys.kpi.lastRun"),
                value: thousands(i64::from(view.series_built)),
                sub: i18n.t("console.recsys.kpi.lastRunSub"),
            }
        }
    }
}

/// How far a running build has got.
///
/// Shown only while one holds the claim. Before this the panel said `full:embedding` and nothing
/// else, so the difference between a build ten seconds in and one wedged for an hour was
/// invisible — an operator's only recourse was to watch the coverage counts and guess. Three
/// facts answer it: which stage of how many, how far through that stage, and how long the run
/// has been going.
///
/// The bar is omitted rather than faked when the stage published no total: `full:vocabulary`,
/// `full:basis` and `full:index` are single database operations with no per-series progress to
/// report, and a bar that sat at zero through them would read as a stall.
#[component]
fn BuildProgress(stage: String, done: i32, total: i32, started_at: Option<String>) -> Element {
    let i18n = use_i18n();

    let position = FULL_STAGES.iter().position(|name| *name == stage);
    let step = position.map(|index| {
        i18n.args(
            "console.recsys.stageStep",
            &[
                ("n", &(index + 1).to_string()),
                ("of", &FULL_STAGES.len().to_string()),
            ],
        )
    });
    let elapsed = started_at
        .as_deref()
        .map(|at| rel_time(i18n, Some(at)))
        .unwrap_or_default();

    // Integer percent, clamped: `stage_total` is read once at the top of a stage and `done`
    // counts up against it, so a catalogue that grew mid-run can overshoot it.
    let percent = if total > 0 {
        Some((i64::from(done) * 100 / i64::from(total)).clamp(0, 100))
    } else {
        None
    };

    rsx! {
        div { style: "margin-bottom:14px;",
            div { class: "ik-flex", style: "gap:10px;align-items:baseline;flex-wrap:wrap;margin-bottom:6px;",
                span { class: "ik-mono", style: "font-size:12.5px;", "{stage}" }
                if let Some(step) = step {
                    span { class: "ik-mono ik-muted", style: "font-size:11.5px;", "{step}" }
                }
                span { class: "ik-mono ik-muted", style: "font-size:11.5px;",
                    if let Some(percent) = percent {
                        {
                            i18n.args(
                                "console.recsys.progressOf",
                                &[
                                    ("done", &thousands(i64::from(done))),
                                    ("total", &thousands(i64::from(total))),
                                    ("percent", &percent.to_string()),
                                ],
                            )
                        }
                    } else {
                        {i18n.args("console.recsys.progressWorking", &[("done", &thousands(i64::from(done)))])}
                    }
                }
                if !elapsed.is_empty() {
                    span { class: "ik-mono ik-muted", style: "font-size:11.5px;margin-left:auto;",
                        {i18n.args("console.recsys.runningFor", &[("since", &elapsed)])}
                    }
                }
            }
            if let Some(percent) = percent {
                div { class: "ik-progress",
                    role: "progressbar",
                    "aria-valuenow": "{percent}",
                    "aria-valuemin": "0",
                    "aria-valuemax": "100",
                    span { style: "width:{percent}%;" }
                }
            } else {
                // Indeterminate: the stage is doing work this panel cannot measure. Striped
                // rather than empty, so it does not read as a bar stuck at zero.
                div { class: "ik-progress indeterminate", span {} }
            }
        }
    }
}

/// Run a build now. Disabled while one holds the claim — a second request would be refused by
/// the control plane, and offering it invites the operator to think the first one failed.
#[component]
fn RebuildButton(
    mode: RecsysBuildMode,
    label: String,
    hint: String,
    building: bool,
    reload: Reload,
    gate: StepUpGate,
) -> Element {
    let api = api::use_api();
    let busy = use_busy();
    let click = move |_| {
        gate.attempt(move || {
            if !busy.claim() {
                return;
            }
            let client = gate.client(api);
            spawn(async move {
                if let Err(e) = client
                    .rebuild_model()
                    .body(RebuildRequest { mode })
                    .send()
                    .await
                {
                    // The refetch below reports every other failure — health is what says whether
                    // the run happened. A step-up demand leaves health unchanged and so has no
                    // tell of its own.
                    let _refused = gate.refused(api::Refusal::of(&e));
                }
                // Refetch either way: health is what says whether the run happened and how it ended.
                reload.bump();
                busy.release();
            });
        });
    };
    rsx! {
        Button {
            style: "font-size:12.5px;padding:7px 11px;",
            disabled: busy.is_busy() || building,
            title: "{hint}",
            on_click: click,
            Ic { icon: Icon::Refresh, size: 14 }
            "{label}"
        }
    }
}

/// One group heading and its rows. A heading over nothing reads as a failed load, so an empty
/// group renders as nothing at all.
#[component]
fn TunableGroupSection(
    title: String,
    rows: Vec<TunableView>,
    can_write: bool,
    reload: Reload,
    gate: StepUpGate,
) -> Element {
    if rows.is_empty() {
        return rsx! {};
    }
    rsx! {
        div { class: "ik-subhead", style: "margin-top:18px;", "{title}" }
        div { class: "ik-tablewrap",
            for row in rows {
                TunableRow { key: "{row.key}", row, can_write, reload, gate }
            }
        }
    }
}

/// One tuning value: what it does, what it is, and the controls to change it.
#[component]
fn TunableRow(row: TunableView, can_write: bool, reload: Reload, gate: StepUpGate) -> Element {
    let i18n = use_i18n();
    let busy = use_busy();
    let outcome = use_outcome();

    let key = row.key.to_string();
    let value = row.value;
    let kind = row.kind;
    let mut draft = use_signal(|| format_value(value, kind));
    let mut note = use_signal(String::new);

    // Re-seed when the server's value moves under the editor — a reset, or another operator's
    // write. Without this a reset leaves the field showing the number that was just withdrawn.
    use_effect(use_reactive!(|(value, kind)| {
        draft.set(format_value(value, kind));
    }));

    let changed = row.overridden && (value - row.default_value).abs() > f64::EPSILON;
    let parsed = draft.read().trim().parse::<f64>().ok();
    let in_range = parsed.is_some_and(|n| n >= row.min && n <= row.max);
    let dirty = parsed.is_some_and(|n| (n - value).abs() > f64::EPSILON);

    rsx! {
        div { class: "ik-row", style: "align-items:flex-start;",
            div { class: "grow",
                div { class: "ik-flex", style: "gap:8px;align-items:center;flex-wrap:wrap;",
                    strong { style: "font-size:13px;", "{row.title}" }
                    AppliesPill { applies: row.applies }
                    if changed {
                        Pill {
                            tone: Tone::Accent,
                            {i18n.t("console.recsys.changed")}
                        }
                    }
                    if row.privacy_floor {
                        Pill {
                            class: "star",
                            title: i18n.t("console.recsys.privacyFloorHint"),
                            {i18n.t("console.recsys.privacyFloor")}
                        }
                    }
                }
                div { class: "ik-mono ik-muted", style: "font-size:11px;margin-top:2px;", "{key}" }
                p { class: "ik-muted", style: "font-size:12px;margin:6px 0 0;max-width:74ch;",
                    "{row.description}"
                }
                div { class: "ik-mono ik-muted", style: "font-size:11px;margin-top:4px;",
                    {
                        i18n.args(
                            "console.recsys.bounds",
                            &[
                                ("min", &format_value(row.min, row.kind)),
                                ("max", &format_value(row.max, row.kind)),
                                ("default", &format_value(row.default_value, row.kind)),
                            ],
                        )
                    }
                }
                if let Some(stored) = row.note.clone() {
                    p { style: "font-size:12px;margin:4px 0 0;", "“{stored}”" }
                }
                if let Some(by) = row.updated_by.clone() {
                    div { class: "ik-mono ik-muted", style: "font-size:11px;margin-top:2px;",
                        {
                            let when = iso_date(row.updated_at.as_deref()).to_owned();
                            i18n.args("console.recsys.changedBy", &[("user", &by), ("date", &when)])
                        }
                    }
                }
                OutcomeLine { outcome: outcome.read().clone() }
                if parsed.is_some() && !in_range {
                    ErrorLine { message: i18n.t("console.recsys.outOfRange") }
                }
            }

            if can_write {
                div { class: "ik-flex", style: "gap:6px;flex-shrink:0;align-items:flex-start;",
                    div { style: "display:flex;flex-direction:column;gap:6px;width:190px;",
                        input {
                            class: "ik-input",
                            style: "font-family:var(--font-mono);font-size:13px;padding:7px 10px;",
                            r#type: "text",
                            inputmode: "decimal",
                            "aria-label": i18n.args("console.recsys.valueLabel", &[("title", &row.title)]),
                            value: "{draft}",
                            oninput: move |event| draft.set(event.value()),
                        }
                        input {
                            class: "ik-input",
                            style: "font-size:12px;padding:7px 10px;",
                            r#type: "text",
                            placeholder: i18n.t("console.recsys.notePlaceholder"),
                            "aria-label": i18n.t("console.recsys.notePlaceholder"),
                            value: "{note}",
                            oninput: move |event| note.set(event.value()),
                        }
                    }
                    SaveButton {
                        tunable: key.clone(),
                        value: parsed.unwrap_or(value),
                        note,
                        enabled: dirty && in_range,
                        busy,
                        outcome,
                        reload,
                        gate,
                    }
                    // Only when there is an override to withdraw; otherwise reset does nothing.
                    if row.overridden {
                        ResetButton { tunable: key.clone(), busy, outcome, reload, gate }
                    }
                }
            } else {
                span { class: "ik-mono", style: "font-size:13px;flex:none;",
                    {format_value(value, row.kind)}
                }
            }
        }
    }
}

/// When a change to this value actually reaches a reader.
///
/// Shown on every row because it is this surface's most likely failure: an operator raises a
/// value baked into stored vectors, sees no change, and concludes the page is broken.
#[component]
fn AppliesPill(applies: Applies) -> Element {
    let i18n = use_i18n();
    let (class, key) = match applies {
        Applies::Immediately => ("ik-pill jade", "console.recsys.applies.immediately"),
        Applies::NextBuild => ("ik-pill", "console.recsys.applies.nextBuild"),
        Applies::NextFullBuild => ("ik-pill star", "console.recsys.applies.nextFullBuild"),
    };
    rsx! {
        span { class: "{class}", {i18n.t(key)} }
    }
}

/// Record an explicit decision for one value.
#[component]
fn SaveButton(
    tunable: String,
    value: f64,
    note: Signal<String>,
    enabled: bool,
    busy: Busy,
    outcome: Signal<Outcome>,
    reload: Reload,
    gate: StepUpGate,
) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let mut outcome = outcome;
    let mut note = note;

    let click = move |_| {
        let key = tunable.clone();
        gate.attempt(move || {
            if !busy.claim() {
                return;
            }
            let key = key.clone();
            let written = note.peek().trim().to_owned();
            let client = gate.client(api);
            outcome.set(None);
            spawn(async move {
                let result = client
                    .set_tunable()
                    .key(key)
                    .body(SetTunable {
                        value,
                        note: (!written.is_empty()).then_some(written),
                    })
                    .send()
                    .await;
                match result {
                    Ok(_) => {
                        outcome.set(Some(Ok(i18n.t("console.recsys.saved"))));
                        note.set(String::new());
                    }
                    // The server's own sentence: a refused write carries the rule it broke, and
                    // "the request was rejected" would leave the operator retrying it.
                    Err(e) => {
                        if !gate.refused(api::Refusal::of(&e)) {
                            let message = api::problem_detail(&e)
                                .unwrap_or_else(|| api::guarded_error(i18n, e));
                            outcome.set(Some(Err(message)));
                        }
                    }
                }
                busy.release();
                reload.bump();
            });
        });
    };

    rsx! {
        Button {
            tone: Tone::Primary,
            style: "flex:none;",
            disabled: busy.is_busy() || !enabled,
            on_click: click,
            {i18n.t("common.save")}
        }
    }
}

/// Drop the stored override so the value follows the compiled default.
///
/// Distinct from writing that same number, which records a decision that would survive a future
/// change of the default — so this is its own control rather than a "type the default" hint.
#[component]
fn ResetButton(
    tunable: String,
    busy: Busy,
    outcome: Signal<Outcome>,
    reload: Reload,
    gate: StepUpGate,
) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let mut outcome = outcome;

    let click = move |_| {
        let key = tunable.clone();
        gate.attempt(move || {
            if !busy.claim() {
                return;
            }
            let key = key.clone();
            let client = gate.client(api);
            outcome.set(None);
            spawn(async move {
                if let Err(e) = client.reset_tunable().key(key).send().await {
                    if !gate.refused(api::Refusal::of(&e)) {
                        let message =
                            api::problem_detail(&e).unwrap_or_else(|| api::guarded_error(i18n, e));
                        outcome.set(Some(Err(message)));
                    }
                }
                busy.release();
                reload.bump();
            });
        });
    };

    rsx! {
        Button {
            style: "flex:none;",
            disabled: busy.is_busy(),
            title: i18n.t("console.recsys.resetHint"),
            on_click: click,
            Ic { icon: Icon::Refresh, size: 14 }
        }
    }
}

/// Render a tuning value the way its kind means it.
///
/// Whole numbers for the kinds the pipeline rounds anyway, two decimals for the fractional ones
/// — a shelf size shown as `12.00` invites an operator to wonder what the fraction does.
fn format_value(value: f64, kind: TunableKind) -> String {
    match kind {
        TunableKind::Count | TunableKind::Days | TunableKind::Seconds => format!("{value:.0}"),
        TunableKind::Ratio | TunableKind::Weight => format!("{value:.2}"),
    }
}

#[cfg(test)]
mod tests {
    use super::{format_value, GROUPS};
    use crate::wire::types::TunableKind;

    /// The tuning page must show every group the API publishes, and word each one.
    ///
    /// `GROUPS` is hand-maintained (the generated client cannot enumerate a schema enum), and a
    /// group missing from it is not an error anywhere: its tunables simply never render, so a
    /// whole section of the registry becomes unreachable from the console while the page still
    /// looks complete. Read against the committed `openapi.json`, the only artefact that
    /// connects this workspace to the API's.
    #[test]
    fn every_published_group_is_offered_and_worded() {
        const SPEC: &str = include_str!("../../../../../openapi.json");
        let spec: serde_json::Value = serde_json::from_str(SPEC).expect("openapi.json parses");

        let mut published: Vec<String> = spec["components"]["schemas"]["TunableGroup"]["enum"]
            .as_array()
            .expect("the document declares the TunableGroup vocabulary")
            .iter()
            .map(|v| v.as_str().expect("group tokens are strings").to_owned())
            .collect();
        let mut offered: Vec<String> = GROUPS.iter().map(|(group, _)| group.to_string()).collect();

        published.sort();
        offered.sort();
        assert_eq!(
            offered, published,
            "the tuning page groups differ from the set the API publishes; add the missing \
             variant to `GROUPS` and word it in the catalogue"
        );

        for (group, key) in GROUPS {
            assert!(
                crate::i18n::has_key(key),
                "`{group}` is offered but `{key}` is not in the catalogue"
            );
        }
    }

    /// A count shown with decimals reads as a value the pipeline honours fractionally, which
    /// none of the counted kinds do.
    #[test]
    fn counted_kinds_render_whole_and_fractional_ones_do_not() {
        assert_eq!(format_value(12.0, TunableKind::Count), "12");
        assert_eq!(format_value(21600.0, TunableKind::Seconds), "21600");
        assert_eq!(format_value(0.7, TunableKind::Ratio), "0.70");
        assert_eq!(format_value(-0.6, TunableKind::Weight), "-0.60");
    }
}
