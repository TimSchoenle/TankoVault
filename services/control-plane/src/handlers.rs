//! The internal HTTP handlers the API calls: scan triggers, merge sweeps, the merge policy and
//! model builds.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Deserialize;
use tankovault_contracts::admin::{
    MergeFullSweepView, MergePolicyView, RecsysBuildMode, RecsysBuildView, ScanTriggeredView,
};
use tankovault_control_plane::recsys;
use tankovault_domain::{Feature, ProviderId, ScanMode, Tunable, UserId};
use tankovault_service::problem::Problem;

use super::{AppState, internal, planning::plan_run};
use crate::{dedupe, policy};

#[derive(Debug, Deserialize)]
pub(super) struct TriggerRequest {
    /// If omitted, plan for all active providers.
    provider_id: Option<ProviderId>,
    mode: ScanMode,
}

// Uses `tankovault_contracts::admin::ScanTriggeredView`, not a private struct —
// `services/api` republishes this body verbatim.

/// Plan a run for one provider, or for every active provider when the request names none.
///
/// Coalesced exactly as the scheduler's sweeps are ([`plan_run`]): a provider already scanning in
/// this mode answers with that run's id rather than queueing a second identical run behind it, so
/// a repeated "Scan now" points the caller at the run in progress instead of clogging the queue.
pub(super) async fn trigger_scan(
    State(state): State<AppState>,
    Json(req): Json<TriggerRequest>,
) -> Result<Json<ScanTriggeredView>, Problem> {
    // Repeats the API's check: this endpoint is reachable by anything on the internal
    // network, so the switch must hold here too, not only at the component in front of it.
    if req.mode == ScanMode::Full && !state.features.is_enabled(Feature::ScanningFull) {
        return Err(Problem::new(
            StatusCode::NOT_FOUND,
            "feature_disabled",
            "full catalogue scans are switched off",
        ));
    }

    let providers = match req.provider_id {
        Some(id) => vec![
            tankovault_db::repo::providers::get(&state.pool, id)
                .await
                .map_err(internal)?,
        ],
        None => tankovault_db::repo::providers::list(&state.pool)
            .await
            .map_err(internal)?
            .into_iter()
            .filter(|p| p.state == tankovault_domain::ProviderState::Active)
            .collect(),
    };

    let mut run_ids = Vec::new();
    for provider in &providers {
        let run_id = plan_run(&state, provider, req.mode)
            .await
            .map_err(internal)?;
        run_ids.push(run_id);
    }
    Ok(Json(ScanTriggeredView { run_ids }))
}

/// Runs one duplicate-reconciliation sweep on demand — to preview a `matching.auto_merge`
/// change or clear a backlog without waiting for the schedule.
///
/// Not leader-gated (runs on the replica asked), but still gated on `scanning.auto_merge`,
/// since the switch must hold at the component doing the work.
pub(super) async fn trigger_merge_sweep(
    State(state): State<AppState>,
    body: Option<Json<MergeSweepRequest>>,
) -> Result<Json<tankovault_contracts::admin::MergeSweepView>, Problem> {
    if !state.features.is_enabled(Feature::ScanningAutoMerge) {
        return Err(Problem::new(
            StatusCode::NOT_FOUND,
            "feature_disabled",
            "automatic duplicate merging is switched off",
        ));
    }
    let actor = body.and_then(|Json(b)| b.actor);
    // Always the full scope: an operator asking for a sweep is asking it to look for duplicates,
    // not only to re-score the ones already recorded.
    let report = dedupe::sweep(
        &state.pool,
        policy::thresholds(&state.matching, &state.tunables),
        state.merge_budget,
        dedupe::SweepScope::Full,
        actor,
    )
    .await
    .map_err(internal)?
    .report;
    tracing::info!(
        examined = report.pairs_examined,
        auto_merged = report.auto_merged,
        queued = report.queued,
        withdrawn = report.withdrawn,
        deferred = report.deferred,
        chains_deferred = report.chains_deferred,
        "duplicate sweep (on demand) complete"
    );
    Ok(Json(report))
}

/// Starts an exhaustive duplicate sweep and answers at once; the run itself is detached.
///
/// Not leader-gated (it runs on the replica asked) but still gated on `scanning.auto_merge`,
/// since the switch has to hold at the component doing the work. The run's claim is the mutual
/// exclusion: a request arriving while one is live answers `started: false` rather than putting
/// a second run's merges on top of the ceiling an operator already authorised.
pub(super) async fn trigger_full_merge_sweep(
    State(state): State<AppState>,
    body: Option<Json<MergeSweepRequest>>,
) -> Result<Json<MergeFullSweepView>, Problem> {
    if !state.features.is_enabled(Feature::ScanningAutoMerge) {
        return Err(Problem::new(
            StatusCode::NOT_FOUND,
            "feature_disabled",
            "automatic duplicate merging is switched off",
        ));
    }
    let actor = body.and_then(|Json(b)| b.actor);
    let started = dedupe::sweep_all_detached(
        &state.pool,
        policy::thresholds(&state.matching, &state.tunables),
        state.merge_budget,
        actor,
    )
    .await
    .map_err(internal)?;
    tracing::info!(started, "exhaustive duplicate sweep requested");
    Ok(Json(MergeFullSweepView { started }))
}

/// The automatic-merge policy as this service resolves it: the configured `matching` block with
/// every stored override applied.
///
/// Served from here rather than assembled in the API because the configured baseline is this
/// image's, and the console has to be able to say what resetting a knob would return it to.
pub(super) async fn read_merge_policy(
    State(state): State<AppState>,
) -> Result<Json<Vec<MergePolicyView>>, Problem> {
    policy::view(&state.pool, &state.matching, &state.tunables)
        .await
        .map(Json)
        .map_err(internal)
}

/// Record — or withdraw — an operator's decision about one knob of that policy.
///
/// Answers with the whole policy as it now stands, for the same reason the flags and tuning
/// endpoints do: a console that patched its own row from the request it just sent can show a
/// value this service does not hold.
pub(super) async fn write_merge_policy(
    State(state): State<AppState>,
    Json(req): Json<MergePolicyRequest>,
) -> Result<Json<Vec<MergePolicyView>>, Problem> {
    let tunable: Tunable = req.key.parse().map_err(|_| {
        Problem::new(
            StatusCode::BAD_REQUEST,
            "unknown_tunable",
            "no such automatic-merge setting",
        )
    })?;
    // The key alone does not authorise the write: every other tunable belongs to the
    // recommender, whose surface is behind a different permission in the API.
    if !tunable.is_matching() {
        return Err(Problem::new(
            StatusCode::BAD_REQUEST,
            "unknown_tunable",
            "no such automatic-merge setting",
        ));
    }

    if let Some(refusal) = req.value.and_then(|v| policy::refusal(tunable, v)) {
        return Err(Problem::new(
            StatusCode::BAD_REQUEST,
            "out_of_range",
            &refusal,
        ));
    }

    let note = req.note.as_deref().map(str::trim).filter(|s| !s.is_empty());
    policy::apply(
        &state.pool,
        &state.tunables,
        tunable,
        req.value,
        note,
        req.actor,
    )
    .await
    .map_err(internal)?;

    tracing::info!(
        tunable = %tunable,
        value = ?req.value,
        actor = %req.actor.as_uuid(),
        "automatic-merge policy changed"
    );
    read_merge_policy(State(state)).await
}

/// Start one model build on demand — to apply a `next_build` tuning change, or to bring a model
/// up to date after a long outage, without waiting for the schedule.
///
/// Not leader-gated (it runs on the replica asked) but still gated on
/// `catalogue.recommendations`, since the switch has to hold at the component doing the work.
/// The build's own claim is the mutual exclusion: a run that arrives while another holds it
/// answers `started: false` rather than queueing behind it.
///
/// Answers as soon as the claim is taken, and the build itself runs detached. It used to be
/// awaited here, which meant the request timeout decided how long a build was allowed to
/// take — see [`recsys::build_detached`]. Progress and the outcome are on
/// `GET /v1/admin/recommendations/health`, which the console already polls.
pub(super) async fn trigger_recsys_build(
    State(state): State<AppState>,
    Json(req): Json<RecsysBuildRequest>,
) -> Result<Json<RecsysBuildView>, Problem> {
    if !state.features.is_enabled(Feature::CatalogueRecommendations) {
        return Err(Problem::new(
            StatusCode::NOT_FOUND,
            "feature_disabled",
            "recommendations are switched off",
        ));
    }

    let full = req.mode == RecsysBuildMode::Full;
    let tuning = recsys::BuildTuning::read(
        &state.tunables,
        state.recsys_batch,
        state.recsys_incremental_max,
    );
    let generation = recsys::build_detached(&state.pool, tuning, full)
        .await
        .map_err(internal)?;

    let view = generation.map_or_else(RecsysBuildView::default, |generation| RecsysBuildView {
        started: true,
        generation,
    });
    tracing::info!(
        full,
        started = view.started,
        generation = view.generation,
        "recommendation model build (on demand) started"
    );
    Ok(Json(view))
}

#[derive(Debug, Deserialize)]
pub(super) struct RecsysBuildRequest {
    mode: RecsysBuildMode,
}

/// One knob of the automatic-merge policy, as the API forwards an operator's decision.
#[derive(Debug, Deserialize)]
pub(super) struct MergePolicyRequest {
    /// The `matching.*` key. Anything else is refused, whatever the caller's permission.
    key: String,
    /// `None` withdraws the override, returning the knob to this deployment's configuration.
    value: Option<f64>,
    note: Option<String>,
    /// The operator who decided it, recorded on the row so the page can say who last did.
    actor: UserId,
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct MergeSweepRequest {
    /// The operator who asked, recorded against every candidate the sweep resolves so an
    /// automatic merge is attributable to a person rather than only to the schedule.
    #[serde(default)]
    actor: Option<tankovault_domain::UserId>,
}
