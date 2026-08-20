//! Schedules and plans scans; workers execute. Exposes `POST /internal/scans` for the
//! API's "Scan now" and runs a background scheduler that periodically triggers fast and
//! full scans of active providers.

mod aggregator;
mod cooldown;
mod dedupe;
mod leader;
mod policy;
mod reconcile;

use tankovault_control_plane::config::{Config, SchedulerConfig};
use tankovault_control_plane::recsys;
use tankovault_service::metrics::names::{
    RECSYS_BUILD_DURATION, RECSYS_BUILD_SERIES, RECSYS_MODEL_SERIES,
};

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;
use tankovault_bus::Bus;
use tankovault_contracts::admin::{
    MergeFullSweepView, MergePolicyView, RecsysBuildMode, RecsysBuildView, ScanTriggeredView,
};
use tankovault_contracts::{ScanTaskMessage, TaskKind};
use tankovault_db::PgPool;
use tankovault_domain::{
    Feature, Provider, ProviderId, RunState, ScanMode, ScanRunId, Tunable, UserId,
};
use tankovault_service::health::PostgresCheck;
use tankovault_service::problem::Problem;
use tankovault_service::{
    CancellationToken, FeatureGate, Health, HttpStack, InternalAuth, InternalRoute,
    MetricsRegistry, PostgresFlagSource, PostgresTunableSource, RateLimiter, RouteClassifier,
    RouteTable, TunableSet,
};

/// Trigger a scan run for one provider, or for every enabled provider.
const SCANS: &str = "/internal/scans";
/// Sweep the catalogue for duplicate series and merge what passes the threshold.
const MERGE_SWEEP: &str = "/internal/merge-sweep";
/// Start an exhaustive sweep — rounds of [`MERGE_SWEEP`] until every shortlist is walked out.
const MERGE_SWEEP_ALL: &str = "/internal/merge-sweep-all";
/// Read the automatic-merge policy the sweep applies.
const MERGE_POLICY: &str = "/internal/merge-policy";
/// Rebuild the recommendation model.
const RECSYS_BUILD: &str = "/internal/recsys-build";

/// Who may reach each of this service's privileged routes.
///
/// All three are operator actions the console fronts, so `api` is the only caller: nothing else
/// in the tier has a reason to start a scan, and before per-caller identity every service that
/// held the shared token could start one.
static INTERNAL_ROUTES: &[InternalRoute] = &[
    InternalRoute {
        method: axum::http::Method::POST,
        path: SCANS,
        callers: &["api"],
    },
    InternalRoute {
        method: axum::http::Method::POST,
        path: MERGE_SWEEP,
        callers: &["api"],
    },
    InternalRoute {
        method: axum::http::Method::POST,
        path: MERGE_SWEEP_ALL,
        callers: &["api"],
    },
    InternalRoute {
        method: axum::http::Method::GET,
        path: MERGE_POLICY,
        callers: &["api"],
    },
    InternalRoute {
        method: axum::http::Method::POST,
        path: MERGE_POLICY,
        callers: &["api"],
    },
    InternalRoute {
        method: axum::http::Method::POST,
        path: RECSYS_BUILD,
        callers: &["api"],
    },
];

// `Clone` so the scheduler loop can own a copy: the config now lives behind an `Arc` shared
// with the reload supervisor, so it cannot be moved out of.

#[derive(Clone)]
struct AppState {
    pool: PgPool,
    bus: Bus,
    /// The operator's runtime switches, consulted per sweep rather than at boot so an
    /// incident-time toggle takes effect without a redeploy.
    features: FeatureGate,
    /// The recommender's tuning, read at the start of each build so a change reaches the next
    /// run without a redeploy.
    tunables: TunableSet,
    /// The canonicalisation policy the duplicate sweep applies.
    matching: tankovault_config::MatchingConfig,
    /// Series per streamed batch in a model build. Configuration, not tuning (§8.1): it moves
    /// peak memory and nothing about the output.
    recsys_batch: i64,
    /// Ceiling on one incremental model build.
    recsys_incremental_max: i64,
    /// How much work one full duplicate sweep may do — including how many series it may delete.
    merge_budget: dedupe::SweepBudget,
    /// The same for a rotation-only pass, whose merge ceiling is scaled by its cadence so that
    /// running it more often does not raise how much the sweeps may delete per hour.
    rotation_budget: dedupe::SweepBudget,
    /// How long an unfinished run suppresses another for the same provider and mode.
    run_stale_after: Duration,
    /// How a provider that keeps failing is backed off, per scan mode.
    fast_backoff: cooldown::ScanBackoff,
    full_backoff: cooldown::ScanBackoff,
}

impl AppState {
    /// The backoff policy for `mode`.
    const fn backoff(&self, mode: ScanMode) -> cooldown::ScanBackoff {
        match mode {
            ScanMode::Fast => self.fast_backoff,
            ScanMode::Full => self.full_backoff,
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // First, before anything can build a rustls configuration: rustls cannot choose a provider
    // for itself in this graph and panics instead of erroring. See `tankovault_service::crypto`.
    tankovault_service::install_crypto_provider();

    // Runs before config/telemetry: `scratch` images have no shell or wget, so the binary
    // must probe itself for Docker's HEALTHCHECK.
    if tankovault_service::healthcheck::requested() {
        let cfg: Config = tankovault_config::load()?;
        tankovault_service::run_healthcheck_and_exit(&cfg.bind_addr);
    }

    let boot = tankovault_config::load_watched::<Config>()?;
    // Both are process-global and installed once, which is why `telemetry.*` and `metrics.*`
    // are the two blocks a configuration reload cannot apply.
    tankovault_service::init_tracing(&boot.value.telemetry)?;
    let metrics =
        MetricsRegistry::install(&boot.value.metrics, &boot.value.telemetry.service_name)?;
    let shutdown = tankovault_service::install_shutdown();
    // Outside the reloadable runtime so a reload does not rebind the scrape listener.
    tankovault_service::spawn_metrics_server(metrics.clone(), shutdown.clone());

    tankovault_service::run_reloading(boot, &shutdown, |cfg, generation| {
        serve_once(cfg, metrics.clone(), generation)
    })
    .await
}

/// Build and run everything a configuration change rebuilds: the pool, the bus and Redis
/// connections, the scheduler and aggregator loops, the router and the listener.
///
/// Returns when `shutdown` is cancelled — by the OS signal, or by the supervisor because the
/// configuration changed and this runtime is being replaced. The background loops take the
/// same token, so they stop with the runtime that spawned them rather than accumulating one
/// scheduler per reload.
async fn serve_once(
    cfg: Arc<Config>,
    metrics: MetricsRegistry,
    shutdown: CancellationToken,
) -> anyhow::Result<()> {
    // Resolved before anything binds: starting without identity configured would silently serve
    // privileged routes unauthenticated, so the production profile refuses to boot instead.
    let internal_auth = tankovault_service::internal_auth::resolve(&cfg.internal)?;

    let pool = tankovault_db::connect(
        &cfg.database.url,
        cfg.database.max_connections,
        cfg.database.acquire_timeout_secs,
    )
    .await?;
    tankovault_service::metrics::spawn_pool_sampler(pool.clone(), shutdown.clone());

    let bus = Bus::connect(&cfg.nats.url, internal_auth.tls.as_ref()).await?;
    bus.ensure_streams().await?;

    // Loaded before the scheduler starts so the first post-restart sweep respects stored
    // flags rather than briefly running against defaults.
    let features = FeatureGate::new(std::sync::Arc::new(PostgresFlagSource::new(pool.clone())));
    features
        .spawn_refresh(cfg.features.refresh_interval(), shutdown.clone())
        .await;
    let tunables = TunableSet::new(std::sync::Arc::new(PostgresTunableSource::new(
        pool.clone(),
    )));
    tunables
        .spawn_refresh(cfg.features.refresh_interval(), shutdown.clone())
        .await;

    let state = AppState {
        pool: pool.clone(),
        bus: bus.clone(),
        features,
        tunables,
        matching: cfg.matching.clone(),
        merge_budget: merge_budget(&cfg.scheduler),
        rotation_budget: rotation_budget(&cfg.scheduler),
        run_stale_after: Duration::from_secs(cfg.scheduler.run_stale_after_secs),
        fast_backoff: backoff(&cfg.scheduler, ScanMode::Fast),
        full_backoff: backoff(&cfg.scheduler, ScanMode::Full),
        recsys_batch: cfg.scheduler.recsys_batch,
        recsys_incremental_max: cfg.scheduler.recsys_incremental_max,
    };

    // Leader election: only the elected replica runs the scheduler sweeps.
    let redis_client = match &cfg.redis {
        Some(r) => match leader::connect(&r.url).await {
            Ok(c) => Some(c),
            Err(e) => {
                tracing::warn!(error = %e, "redis connect failed; scheduler runs unguarded");
                None
            }
        },
        None => None,
    };
    let leadership = leader::spawn(redis_client);

    // Background scheduler.
    let sched_state = state.clone();
    let sched_leadership = leadership.clone();
    let sched_shutdown = shutdown.clone();
    let sched_config = cfg.scheduler.clone();
    tokio::spawn(async move {
        run_scheduler(sched_state, sched_config, sched_leadership, sched_shutdown).await;
    });

    // Background progress aggregator: finalise runs as their tasks settle and relay one
    // terminal `scan.progress` event so the console SSE need not DB-poll to a conclusion.
    let agg_pool = pool.clone();
    let agg_bus = bus.clone();
    let agg_shutdown = shutdown.clone();
    tokio::spawn(async move {
        if let Err(e) = aggregator::run(agg_pool, agg_bus, agg_shutdown).await {
            tracing::error!(error = %e, "progress aggregator exited");
        }
    });

    // Readiness names both hard dependencies: without Postgres it cannot plan a run, and
    // without NATS it cannot publish one, so a replica missing either must not take work.
    let ready_bus = bus.clone();
    let health = Health::builder()
        .check(PostgresCheck::new(pool))
        .check_fn("nats", move || {
            let bus = ready_bus.clone();
            async move { bus.ping().await.map_err(|e| e.to_string()) }
        })
        .build();

    let limiter = RateLimiter::from_config(&cfg.rate_limit, RouteClassifier::new(), None);
    let app = HttpStack::new(&cfg.security, metrics.clone())
        .with_rate_limit(limiter)
        .with_internal_auth(Some(InternalAuth::new(
            &internal_auth,
            RouteTable(INTERNAL_ROUTES),
        )))
        .apply(
            Router::new()
                .route(SCANS, post(trigger_scan))
                .route(MERGE_SWEEP, post(trigger_merge_sweep))
                .route(MERGE_SWEEP_ALL, post(trigger_full_merge_sweep))
                .route(
                    MERGE_POLICY,
                    get(read_merge_policy).post(write_merge_policy),
                )
                .route(RECSYS_BUILD, post(trigger_recsys_build))
                .with_state(state),
        )
        .merge(tankovault_service::ops_router(health.clone(), metrics));

    tankovault_service::serve_internal(
        &cfg.bind_addr,
        app,
        tankovault_service::probe_router(health),
        &internal_auth,
        shutdown,
    )
    .await?;
    Ok(())
}

// The scheduler's derived budgets, as free functions rather than an inherent `impl`: the config
// type lives in this crate's library — `config-contract` has to reach it — and the types these
// return are the binary's own.
/// The failure backoff for one scan mode.
///
/// The mode's own sweep interval is the unit, so the first step is "skip one sweep" and a
/// deployment that sweeps hourly backs off in hours. A disabled cadence (`0`) has no sweep to
/// skip, so the policy is inert there whatever the ceiling says.
fn backoff(scheduler: &SchedulerConfig, mode: ScanMode) -> cooldown::ScanBackoff {
    let interval = match mode {
        ScanMode::Fast => scheduler.fast_interval_secs,
        ScanMode::Full => scheduler.full_interval_secs,
    };
    cooldown::ScanBackoff {
        interval: Duration::from_secs(interval),
        max: Duration::from_secs(scheduler.failure_backoff_max_secs),
    }
}

const fn merge_budget(scheduler: &SchedulerConfig) -> dedupe::SweepBudget {
    dedupe::SweepBudget {
        pairs: scheduler.merge_sweep_pairs,
        requeue: scheduler.merge_sweep_requeue,
        recheck: scheduler.merge_sweep_recheck,
        max_auto_merges: scheduler.merge_sweep_max_auto_merges,
    }
}

/// The budget for a rotation-only pass.
///
/// Discovery is dropped, and the automatic-merge ceiling is scaled by how much more often
/// this runs than the full sweep does. That ceiling is really a *rate* — the bound on how
/// many rows a bad threshold can delete before an operator looks — so running the rotation
/// four times an hour at the full sweep's per-run ceiling would quintuple the rate without
/// anyone having chosen to. With discovery disabled there is no other sweep to share the rate
/// with, and the rotation carries the whole ceiling.
fn rotation_budget(scheduler: &SchedulerConfig) -> dedupe::SweepBudget {
    let full = merge_budget(scheduler);
    let scaled = i64::try_from(scheduler.merge_sweep_rotation_interval_secs)
        .unwrap_or(i64::MAX)
        .saturating_mul(full.max_auto_merges)
        .checked_div(i64::try_from(scheduler.merge_sweep_interval_secs).unwrap_or(i64::MAX))
        .unwrap_or(full.max_auto_merges);
    dedupe::SweepBudget {
        pairs: 0,
        max_auto_merges: scaled.clamp(1, full.max_auto_merges.max(1)),
        ..full
    }
}

#[derive(Debug, Deserialize)]
struct TriggerRequest {
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
async fn trigger_scan(
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
async fn trigger_merge_sweep(
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
async fn trigger_full_merge_sweep(
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
async fn read_merge_policy(
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
async fn write_merge_policy(
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
async fn trigger_recsys_build(
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
struct RecsysBuildRequest {
    mode: RecsysBuildMode,
}

/// One knob of the automatic-merge policy, as the API forwards an operator's decision.
#[derive(Debug, Deserialize)]
struct MergePolicyRequest {
    /// The `matching.*` key. Anything else is refused, whatever the caller's permission.
    key: String,
    /// `None` withdraws the override, returning the knob to this deployment's configuration.
    value: Option<f64>,
    note: Option<String>,
    /// The operator who decided it, recorded on the row so the page can say who last did.
    actor: UserId,
}

#[derive(Debug, Default, Deserialize)]
struct MergeSweepRequest {
    /// The operator who asked, recorded against every candidate the sweep resolves so an
    /// automatic merge is attributable to a person rather than only to the schedule.
    #[serde(default)]
    actor: Option<tankovault_domain::UserId>,
}

/// Expand a run into its initial task(s) and dispatch them, unless the provider already has one
/// of this mode in flight — in which case that run's id is returned and nothing is queued.
///
/// ## Why planning is coalesced rather than queued
///
/// Nothing about a scan run bounds how long it takes: a fast scan is one `latest_feed` task that
/// re-ingests every series the feed names, at whatever rate that provider's crawl budget allows.
/// The sweep, by contrast, ticks on a fixed interval. A provider whose fast scan outlasts
/// `fast_interval_secs` therefore gained a second queued run every tick, indefinitely — and those
/// runs are *identical*, since each one re-reads the same feed.
///
/// The worker's per-provider lanes bound the damage but do not stop it: the backlog is served one
/// task at a time, so the provider spends every slot it is given re-reading a feed it has already
/// read, and its lane never empties. The fast tier is drained before the full tier is looked at
/// at all, so one such provider also stops every full scan in the deployment from being served.
///
/// Coalescing removes the cause. A provider has at most one in-flight run per mode, so a scan
/// that is slow simply keeps running, and the tick that would have duplicated it does nothing.
async fn plan_run(
    state: &AppState,
    provider: &Provider,
    mode: ScanMode,
) -> anyhow::Result<ScanRunId> {
    if let Some(run_id) = tankovault_db::repo::scans::in_flight_run(
        &state.pool,
        provider.id,
        mode,
        state.run_stale_after,
    )
    .await?
    {
        tracing::info!(
            %run_id,
            provider = %provider.slug,
            ?mode,
            "skipped planning a scan run; the provider already has one in flight"
        );
        planned(&provider.slug, mode, "coalesced");
        return Ok(run_id);
    }

    let run_id =
        tankovault_db::repo::scans::create_run(&state.pool, Some(provider.id), mode).await?;

    match dispatch_run(state, provider, mode, run_id).await {
        Ok(result) => {
            tracing::info!(%run_id, provider = %provider.slug, ?mode, result, "planned scan run");
            planned(&provider.slug, mode, result);
            Ok(run_id)
        }
        Err(e) => {
            // The run row exists and would read as in flight forever, which now means it would
            // suppress this provider's every later run until it went stale. Failing it here is
            // what keeps one broker hiccup from costing a provider its whole schedule.
            if let Err(fail_err) =
                tankovault_db::repo::scans::finish_run(&state.pool, run_id, RunState::Failed).await
            {
                tracing::warn!(
                    %run_id,
                    provider = %provider.slug,
                    error = %fail_err,
                    next = "the run stays in flight and suppresses this provider's scans until it \
                            passes scheduler.run_stale_after_secs",
                    "could not fail the run whose dispatch failed"
                );
            }
            Err(e)
        }
    }
}

/// Persist and publish `run_id`'s initial task, naming which outcome it was for the log and the
/// counter.
async fn dispatch_run(
    state: &AppState,
    provider: &Provider,
    mode: ScanMode,
    run_id: ScanRunId,
) -> anyhow::Result<&'static str> {
    tankovault_db::repo::scans::start_run(&state.pool, run_id).await?;

    let (kind_str, kind, target) = match mode {
        ScanMode::Full => (
            "catalog_page",
            TaskKind::CatalogPage,
            serde_json::json!({ "page": 1 }),
        ),
        ScanMode::Fast => ("latest_feed", TaskKind::LatestFeed, serde_json::json!({})),
    };

    let Some(task_id) =
        tankovault_db::repo::scans::create_task(&state.pool, run_id, kind_str, &target).await?
    else {
        // A task with this exact target already exists for the run (idempotent replan);
        // the run is already dispatched, so there is nothing further to publish.
        return Ok("duplicate");
    };
    tankovault_db::repo::scans::add_total_tasks(&state.pool, run_id, 1).await?;

    state
        .bus
        .publish_task(&ScanTaskMessage {
            task_id,
            run_id,
            provider_id: provider.id,
            provider_slug: provider.slug.clone(),
            mode,
            kind,
            target,
            traceparent: None,
        })
        .await?;
    Ok("planned")
}

/// Count one planning decision.
///
/// The upstream half of the scan pipeline: `scan_tasks_served_total` on the worker says work
/// is being consumed, and only this says work is being *created*. A scheduler that is alive
/// but planning nothing — a lost leader, a flag left off — is otherwise indistinguishable
/// from an idle deployment.
///
/// `result` is one of `planned`, `duplicate`, `coalesced`, `cooling_down` or `error`. A provider
/// scanning more slowly than it is swept shows up as a steady stream of `coalesced` and no
/// `planned`, which is the signal to look at its crawl budget or its feed size rather than at the
/// scheduler. A steady stream of `cooling_down` is the other shape worth a panel: that provider
/// is not being scanned at all, and the reason is its own failure streak.
fn planned(provider_slug: &str, mode: ScanMode, result: &'static str) {
    metrics::counter!(
        "scan_runs_planned_total",
        "provider" => provider_slug.to_owned(),
        "scan" => mode.as_str(),
        "result" => result,
    )
    .increment(1);
}

/// Periodic scheduler loop.
///
/// Exits on `shutdown` rather than being killed mid-sweep: a severed sweep leaves
/// planned-but-unpublished runs behind, which the aggregator then waits on forever.
async fn run_scheduler(
    state: AppState,
    cfg: SchedulerConfig,
    leadership: leader::Leadership,
    shutdown: tokio_util::sync::CancellationToken,
) {
    if cfg.fast_interval_secs == 0
        && cfg.full_interval_secs == 0
        && cfg.merge_sweep_interval_secs == 0
        && cfg.merge_sweep_rotation_interval_secs == 0
        && cfg.recsys_incremental_interval_secs == 0
        && cfg.recsys_full_interval_secs == 0
        && cfg.reconcile_interval_secs == 0
    {
        tracing::info!("scheduler disabled");
        return;
    }
    let mut fast = interval_or_never(cfg.fast_interval_secs);
    let mut full = interval_or_never(cfg.full_interval_secs);
    let mut merge = interval_or_never(cfg.merge_sweep_interval_secs);
    let mut merge_rotation = interval_or_never(cfg.merge_sweep_rotation_interval_secs);
    let mut repair = interval_or_never(cfg.reconcile_interval_secs);
    let mut recsys_incremental = interval_or_never(cfg.recsys_incremental_interval_secs);
    let mut recsys_full = interval_or_never(cfg.recsys_full_interval_secs);

    loop {
        // Republished on every tick rather than on acquisition, so a replica that lost the
        // lock without noticing still reports the truth. Summed over the job this must be
        // exactly 1; 0 means nothing is planning scans and the pipeline is quietly stopped.
        metrics::gauge!("scheduler_leader").set(f64::from(u8::from(leadership.is_leader())));
        tokio::select! {
            () = shutdown.cancelled() => {
                tracing::info!("scheduler stopping");
                return;
            }
            () = tick(&mut fast) => maybe_sweep(&state, &leadership, ScanMode::Fast).await,
            () = tick(&mut full) => maybe_sweep(&state, &leadership, ScanMode::Full).await,
            () = tick(&mut merge) => {
                maybe_merge_sweep(&state, &leadership, dedupe::SweepScope::Full).await;
            }
            () = tick(&mut merge_rotation) => {
                maybe_merge_sweep(&state, &leadership, dedupe::SweepScope::Rotation).await;
            }
            () = tick(&mut repair) => maybe_reconcile(&state, &leadership).await,
            () = tick(&mut recsys_incremental) => {
                maybe_recsys_build(&state, &cfg, &leadership, false).await;
            }
            () = tick(&mut recsys_full) => {
                maybe_recsys_build(&state, &cfg, &leadership, true).await;
            }
        }
    }
}

/// Runs a model build only when this replica holds leadership *and* recommendations are on.
///
/// Leadership matters for the same reason it does for the duplicate sweep, and for one more:
/// a build claims `rec_build_state` and two replicas racing would have one of them do nothing
/// while believing it had run. The claim is the real mutual exclusion; leadership keeps the
/// wasted attempt from happening at all.
async fn maybe_recsys_build(
    state: &AppState,
    cfg: &SchedulerConfig,
    leadership: &leader::Leadership,
    full: bool,
) {
    let kind = if full { "full" } else { "incremental" };
    if !state.features.is_enabled(Feature::CatalogueRecommendations) {
        tracing::debug!(
            kind,
            "skipping model build; recommendations are switched off"
        );
        return;
    }
    if !leadership.is_leader() {
        tracing::debug!(kind, "skipping model build; not scheduler leader");
        return;
    }

    let tuning = recsys::BuildTuning::read(
        &state.tunables,
        cfg.recsys_batch,
        cfg.recsys_incremental_max,
    );
    let started = std::time::Instant::now();
    match recsys::build(&state.pool, tuning, full).await {
        Ok(Some(report)) => {
            tracing::info!(
                kind,
                generation = report.generation,
                series = report.series_built,
                vocabulary = report.vocabulary,
                dims = report.dense_dims,
                "recommendation model built"
            );
            metrics::counter!(RECSYS_BUILD_SERIES, "stage" => kind, "result" => "built")
                .increment(u64::try_from(report.series_built).unwrap_or(0));
            // A gauge takes an `f64`; the count is an `i64` that cannot be negative, and a
            // catalogue large enough to lose precision here is far past anything this build
            // completes in a day.
            #[expect(
                clippy::cast_precision_loss,
                reason = "series counts are far below f64's exact-integer range"
            )]
            metrics::gauge!(RECSYS_MODEL_SERIES, "table" => "series_embedding")
                .set(report.series_built.max(0) as f64);
        }
        // Not a failure: another replica holds the claim and is doing this run's work.
        Ok(None) => tracing::debug!(kind, "model build already in progress"),
        Err(e) => {
            tracing::warn!(kind, error = %e, "recommendation model build failed");
            metrics::counter!(RECSYS_BUILD_SERIES, "stage" => kind, "result" => "failed")
                .increment(1);
        }
    }
    metrics::histogram!(RECSYS_BUILD_DURATION, "stage" => kind)
        .record(started.elapsed().as_secs_f64());
}

/// Repairs dispatch that has drifted from the task table, on the leader only.
///
/// Deliberately **not** behind [`Feature::ScanningScheduler`]. That flag stops new scans being
/// planned; it is the switch an operator reaches for during an incident, which is exactly when
/// the runs already in flight most need to be able to finish. A repair creates no new work — it
/// republishes messages for rows that already exist and closes runs that are already over.
async fn maybe_reconcile(state: &AppState, leadership: &leader::Leadership) {
    if !leadership.is_leader() {
        tracing::debug!("skipping reconciliation; not scheduler leader");
        return;
    }
    let started = std::time::Instant::now();
    if let Err(e) = reconcile::pass(state).await {
        tracing::warn!(error = %e, "scan dispatch reconciliation failed");
    }
    metrics::histogram!("scan_reconcile_duration_seconds").record(started.elapsed().as_secs_f64());
}

/// Runs a duplicate sweep only when this replica holds leadership *and* automatic
/// merging is switched on.
///
/// Leadership matters more than for a scan sweep: two replicas merging the same pair
/// race on a destructive transaction, and the loser finds its series already gone.
async fn maybe_merge_sweep(
    state: &AppState,
    leadership: &leader::Leadership,
    scope: dedupe::SweepScope,
) {
    if !state.features.is_enabled(Feature::ScanningAutoMerge) {
        tracing::debug!("skipping duplicate sweep; automatic merging is switched off");
        return;
    }
    if !leadership.is_leader() {
        tracing::debug!("skipping duplicate sweep; not scheduler leader");
        return;
    }
    let budget = match scope {
        dedupe::SweepScope::Full => state.merge_budget,
        dedupe::SweepScope::Rotation => state.rotation_budget,
    };
    let started = std::time::Instant::now();
    let thresholds = policy::thresholds(&state.matching, &state.tunables);
    match dedupe::sweep(&state.pool, thresholds, budget, scope, None).await {
        Ok(dedupe::SweepRun { report, .. }) => {
            tracing::info!(
                examined = report.pairs_examined,
                auto_merged = report.auto_merged,
                queued = report.queued,
                withdrawn = report.withdrawn,
                deferred = report.deferred,
                chains_deferred = report.chains_deferred,
                "duplicate sweep complete"
            );
            // `auto_merged` is the destructive one and the only arm bounded by the merge
            // ceiling, so it is worth watching on its own rather than as a sweep total.
            for (action, count) in [
                ("examined", report.pairs_examined),
                ("auto_merged", report.auto_merged),
                ("queued", report.queued),
                ("withdrawn", report.withdrawn),
                ("deferred", report.deferred),
                ("chains_deferred", report.chains_deferred),
            ] {
                // The report's counts are `i64` and cannot be negative; a saturating
                // conversion keeps a future signed field from wrapping into a huge counter
                // jump, which on a monotonic counter reads as a reset.
                metrics::counter!("merge_sweep_actions_total", "action" => action)
                    .increment(u64::try_from(count).unwrap_or(0));
            }
        }
        Err(e) => tracing::warn!(error = %e, "duplicate sweep failed"),
    }
    // Labelled by scope: a rotation pass and a full sweep have different costs by design, and
    // one histogram over both reads as bimodal noise rather than as either.
    metrics::histogram!(
        "merge_sweep_duration_seconds",
        "scope" => match scope {
            dedupe::SweepScope::Full => "full",
            dedupe::SweepScope::Rotation => "rotation",
        },
    )
    .record(started.elapsed().as_secs_f64());
}

/// Runs a sweep only when this replica holds scheduler leadership *and* scheduled scanning
/// is switched on.
///
/// Checked per sweep, not when the loop is built, so switching `scanning.scheduler` off
/// takes effect on the next tick without a redeploy, and resumes on its own when it's back.
async fn maybe_sweep(state: &AppState, leadership: &leader::Leadership, mode: ScanMode) {
    if !state.features.is_enabled(Feature::ScanningScheduler) {
        tracing::debug!(?mode, "skipping sweep; scheduled scanning is switched off");
        return;
    }
    // A scheduled *full* sweep is the expensive one. `scanning.full` lets an operator stop it
    // while leaving the cheap latest-feed pass running, which is the usual way to back off.
    if mode == ScanMode::Full && !state.features.is_enabled(Feature::ScanningFull) {
        tracing::debug!("skipping sweep; full catalogue scans are switched off");
        return;
    }
    if leadership.is_leader() {
        sweep(state, mode).await;
    } else {
        tracing::debug!(?mode, "skipping sweep; not scheduler leader");
    }
}

/// Whether `provider` is still serving out a cooldown earned by consecutive failed runs.
///
/// Only the **sweep** consults this. An operator pressing "Scan now" is asking a question the
/// backoff exists to stop *us* asking, and the answer they want is the current one — so
/// [`trigger_scan`] goes straight to [`plan_run`] and a manual run also ends the streak the
/// moment it succeeds.
async fn cooling_down(
    state: &AppState,
    provider: &Provider,
    mode: ScanMode,
) -> anyhow::Result<bool> {
    let streak = tankovault_db::repo::scans::failure_streak(&state.pool, provider.id, mode).await?;
    let Some(remaining) = state
        .backoff(mode)
        .remaining(streak, time::OffsetDateTime::now_utc())
    else {
        return Ok(false);
    };

    // At `debug`, not `warn`: once a provider is down this fires every tick, and the failure it
    // reports is already in the triage feed. The counter below is what carries it to a panel.
    tracing::debug!(
        provider = %provider.slug,
        ?mode,
        failures = streak.failures,
        remaining_secs = remaining.as_secs(),
        "skipping the sweep; provider is backing off after consecutive failed runs"
    );
    planned(&provider.slug, mode, "cooling_down");
    Ok(true)
}

async fn sweep(state: &AppState, mode: ScanMode) {
    let started = std::time::Instant::now();
    let providers = match tankovault_db::repo::providers::list(&state.pool).await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "scheduler: failed to list providers");
            return;
        }
    };
    for provider in providers
        .iter()
        .filter(|p| p.state == tankovault_domain::ProviderState::Active)
    {
        match cooling_down(state, provider, mode).await {
            Ok(false) => {}
            Ok(true) => continue,
            Err(e) => {
                // The streak is an optimisation on politeness, not a precondition for scanning:
                // a provider must not stop being scanned because one read failed.
                tracing::warn!(
                    provider = %provider.slug,
                    error = %e,
                    next = "planning the run anyway; the backoff resumes on the next sweep",
                    "scheduler: could not read the provider's failure streak"
                );
            }
        }
        if let Err(e) = plan_run(state, provider, mode).await {
            tracing::warn!(provider = %provider.slug, error = %e, "scheduler: plan failed");
            planned(&provider.slug, mode, "error");
        }
    }
    metrics::histogram!("scheduler_sweep_duration_seconds", "scan" => mode.as_str())
        .record(started.elapsed().as_secs_f64());
}

fn interval_or_never(secs: u64) -> Option<tokio::time::Interval> {
    (secs > 0).then(|| tokio::time::interval(Duration::from_secs(secs)))
}

async fn tick(maybe: &mut Option<tokio::time::Interval>) {
    match maybe {
        Some(i) => {
            i.tick().await;
        }
        // Never resolves, so `select!` ignores a disabled cadence.
        None => std::future::pending::<()>().await,
    }
}

/// Logs the cause and answers with an opaque `500` [`Problem`].
///
/// Never put the raw error `Display` on the wire: it can carry connection strings or SQL.
fn internal<E: std::fmt::Display>(e: E) -> Problem {
    tracing::error!(error = %e, "control-plane request failed");
    Problem::internal()
}

#[cfg(test)]
mod tests {
    use super::{SchedulerConfig, merge_budget, rotation_budget};
    use tankovault_control_plane::config::default_merge_sweep_max_auto_merges;

    /// Running the queue rotation more often must not raise how many series the sweeps may
    /// delete per hour.
    ///
    /// `merge_sweep_max_auto_merges` is the only bound on a destructive background action, and it
    /// is written per run. Splitting the sweep into an hourly discovery pass and a frequent
    /// rotation pass silently multiplied that bound by the number of rotations in an hour â€” the
    /// ceiling would still have read `200` in the configuration while permitting five times as
    /// many deletions between two looks at the catalogue. The rotation's share is therefore
    /// scaled by its cadence, and this pins the arithmetic.
    #[test]
    fn a_rotation_pass_does_not_multiply_the_hourly_merge_ceiling() {
        let cfg = SchedulerConfig::default();
        let per_hour = 3600 / cfg.merge_sweep_rotation_interval_secs;
        let rotation = rotation_budget(&cfg);

        assert_eq!(
            rotation.max_auto_merges * i64::try_from(per_hour).expect("a small count"),
            merge_budget(&cfg).max_auto_merges,
            "four rotations at the scaled ceiling must equal one full sweep at the configured one",
        );
        assert_eq!(
            rotation.pairs, 0,
            "a rotation pass does no discovery, which is the expensive half",
        );
        assert_eq!(rotation.requeue, cfg.merge_sweep_requeue);
        assert_eq!(rotation.recheck, cfg.merge_sweep_recheck);
    }

    /// With discovery switched off the rotation is the only sweep, so it carries the whole
    /// ceiling rather than a share of a cadence that never runs.
    #[test]
    fn the_rotation_carries_the_whole_ceiling_when_discovery_is_disabled() {
        let cfg = SchedulerConfig {
            merge_sweep_interval_secs: 0,
            ..SchedulerConfig::default()
        };
        assert_eq!(
            rotation_budget(&cfg).max_auto_merges,
            default_merge_sweep_max_auto_merges(),
        );
    }

    /// A rotation may always merge at least one pair.
    ///
    /// The scaling is integer division, so a rotation interval short enough relative to the
    /// discovery one rounds its share to zero â€” which would not be a conservative ceiling but a
    /// rotation that can re-score the queue forever and never act on it.
    #[test]
    fn a_rotation_may_always_merge_at_least_one_pair() {
        let cfg = SchedulerConfig {
            merge_sweep_interval_secs: 86_400,
            merge_sweep_rotation_interval_secs: 60,
            merge_sweep_max_auto_merges: 10,
            ..SchedulerConfig::default()
        };
        assert_eq!(rotation_budget(&cfg).max_auto_merges, 1);
    }
}
