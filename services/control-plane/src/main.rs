//! Schedules and plans scans; workers execute. Exposes `POST /internal/scans` for the
//! API's "Scan now" and runs a background scheduler that periodically triggers fast and
//! full scans of active providers.

mod aggregator;
mod dedupe;
mod leader;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;
use tankovault_bus::Bus;
use tankovault_contracts::admin::ScanTriggeredView;
use tankovault_contracts::{ScanTaskMessage, TaskKind};
use tankovault_db::PgPool;
use tankovault_domain::{Feature, Provider, ProviderId, ScanMode, ScanRunId};
use tankovault_service::health::PostgresCheck;
use tankovault_service::problem::Problem;
use tankovault_service::{
    CancellationToken, FeatureGate, Health, HttpStack, MetricsRegistry, PostgresFlagSource,
    RateLimiter, RouteClassifier,
};

#[derive(Debug, Deserialize)]
struct Config {
    database: tankovault_config::DatabaseConfig,
    nats: tankovault_config::NatsConfig,
    telemetry: tankovault_config::TelemetryConfig,
    /// Optional Redis endpoint used for singleton-scheduler leader election. When absent,
    /// this replica is treated as the sole leader (single-instance / local dev).
    #[serde(default)]
    redis: Option<tankovault_config::RedisConfig>,
    #[serde(default)]
    scheduler: SchedulerConfig,
    #[serde(default = "default_bind")]
    bind_addr: String,
    /// Edge hardening for the internal trigger endpoint.
    #[serde(default)]
    security: tankovault_config::SecurityConfig,
    /// Inbound rate limiting on `/internal/scans`, so a stuck caller cannot fan out
    /// unbounded scan runs.
    #[serde(default)]
    rate_limit: tankovault_config::RateLimitConfig,
    /// Prometheus metrics. Togglable; disabling installs no recorder.
    #[serde(default)]
    metrics: tankovault_config::MetricsConfig,
    /// Runtime feature flags — how often this replica re-reads the operator's decisions.
    #[serde(default)]
    features: tankovault_config::FeaturesConfig,
    /// Shared secret every caller must present on `/internal/*`. Triggering scan runs is an
    /// operator action; the endpoint's name is not an access control.
    #[serde(default)]
    internal: tankovault_config::InternalAuthConfig,
    /// The confidence policy for matching series, shared with the worker's ingest and
    /// external sync so no two paths disagree; the duplicate sweep applies it to
    /// existing series.
    #[serde(default)]
    matching: tankovault_config::MatchingConfig,
}

fn default_bind() -> String {
    "0.0.0.0:8081".to_owned()
}

// `Clone` so the scheduler loop can own a copy: the config now lives behind an `Arc` shared
// with the reload supervisor, so it cannot be moved out of.
#[derive(Debug, Clone, Deserialize)]
struct SchedulerConfig {
    /// Seconds between fast-scan sweeps of all active providers. 0 disables.
    #[serde(default = "default_fast_interval")]
    fast_interval_secs: u64,
    /// Seconds between full-scan sweeps. 0 disables (full scans are usually on demand).
    #[serde(default)]
    full_interval_secs: u64,
    /// Seconds between duplicate-reconciliation sweeps. 0 disables.
    ///
    /// Hourly by default: the enrichment the sweep needs (authors, year, alt titles)
    /// happens on the order of hours, not minutes.
    #[serde(default = "default_merge_sweep_interval")]
    merge_sweep_interval_secs: u64,
    /// Newly-blocked duplicate pairs shortlisted per sweep.
    #[serde(default = "default_merge_sweep_pairs")]
    merge_sweep_pairs: i64,
    /// Open queue rows re-scored per sweep, least-recently-scored first.
    #[serde(default = "default_merge_sweep_requeue")]
    merge_sweep_requeue: i64,
    /// Previously-distinct pairs reconsidered per sweep, least-recently-scored first.
    #[serde(default = "default_merge_sweep_recheck")]
    merge_sweep_recheck: i64,
    /// Automatic merges permitted in a single sweep — the only bound on a destructive
    /// background action. Without it, a bad threshold or normalization rule could collapse
    /// the whole catalogue between two scheduler ticks.
    #[serde(default = "default_merge_sweep_max_auto_merges")]
    merge_sweep_max_auto_merges: i64,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            fast_interval_secs: default_fast_interval(),
            full_interval_secs: 0,
            merge_sweep_interval_secs: default_merge_sweep_interval(),
            merge_sweep_pairs: default_merge_sweep_pairs(),
            merge_sweep_requeue: default_merge_sweep_requeue(),
            merge_sweep_recheck: default_merge_sweep_recheck(),
            merge_sweep_max_auto_merges: default_merge_sweep_max_auto_merges(),
        }
    }
}

impl SchedulerConfig {
    const fn merge_budget(&self) -> dedupe::SweepBudget {
        dedupe::SweepBudget {
            pairs: self.merge_sweep_pairs,
            requeue: self.merge_sweep_requeue,
            recheck: self.merge_sweep_recheck,
            max_auto_merges: self.merge_sweep_max_auto_merges,
        }
    }
}

fn default_fast_interval() -> u64 {
    300
}
fn default_merge_sweep_interval() -> u64 {
    3600
}
fn default_merge_sweep_pairs() -> i64 {
    500
}
fn default_merge_sweep_requeue() -> i64 {
    250
}
fn default_merge_sweep_recheck() -> i64 {
    250
}
fn default_merge_sweep_max_auto_merges() -> i64 {
    200
}

#[derive(Clone)]
struct AppState {
    pool: PgPool,
    bus: Bus,
    /// The operator's runtime switches, consulted per sweep rather than at boot so an
    /// incident-time toggle takes effect without a redeploy.
    features: FeatureGate,
    /// The canonicalisation policy the duplicate sweep applies.
    matching: tankovault_config::MatchingConfig,
    /// How much work one duplicate sweep may do — including how many series it may delete.
    merge_budget: dedupe::SweepBudget,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
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
    let metrics = MetricsRegistry::install(&boot.value.metrics)?;
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
    // Resolved before anything binds: starting without a token would silently serve
    // privileged routes unauthenticated, so the production profile refuses to boot instead.
    let internal_token = tankovault_service::internal_auth::resolve(&cfg.internal)?;

    let pool = tankovault_db::connect(
        &cfg.database.url,
        cfg.database.max_connections,
        cfg.database.acquire_timeout_secs,
    )
    .await?;

    let bus = Bus::connect(&cfg.nats.url).await?;
    bus.ensure_streams().await?;

    // Loaded before the scheduler starts so the first post-restart sweep respects stored
    // flags rather than briefly running against defaults.
    let features = FeatureGate::new(std::sync::Arc::new(PostgresFlagSource::new(pool.clone())));
    features
        .spawn_refresh(cfg.features.refresh_interval(), shutdown.clone())
        .await;

    let state = AppState {
        pool: pool.clone(),
        bus: bus.clone(),
        features,
        matching: cfg.matching.clone(),
        merge_budget: cfg.scheduler.merge_budget(),
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
        .with_internal_auth(internal_token)
        .apply(
            Router::new()
                .route("/internal/scans", post(trigger_scan))
                .route("/internal/merge-sweep", post(trigger_merge_sweep))
                .with_state(state),
        )
        .merge(tankovault_service::ops_router(health, metrics));

    tankovault_service::serve(&cfg.bind_addr, app, shutdown).await?;
    Ok(())
}

#[derive(Debug, Deserialize)]
struct TriggerRequest {
    /// If omitted, plan for all active providers.
    provider_id: Option<ProviderId>,
    mode: ScanMode,
}

// Uses `tankovault_contracts::admin::ScanTriggeredView`, not a private struct —
// `services/api` republishes this body verbatim.

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
    let report = dedupe::sweep(&state.pool, &state.matching, state.merge_budget, actor)
        .await
        .map_err(internal)?;
    tracing::info!(
        examined = report.pairs_examined,
        auto_merged = report.auto_merged,
        queued = report.queued,
        withdrawn = report.withdrawn,
        deferred = report.deferred,
        "duplicate sweep (on demand) complete"
    );
    Ok(Json(report))
}

#[derive(Debug, Default, Deserialize)]
struct MergeSweepRequest {
    /// The operator who asked, recorded against every candidate the sweep resolves so an
    /// automatic merge is attributable to a person rather than only to the schedule.
    #[serde(default)]
    actor: Option<tankovault_domain::UserId>,
}

/// Expand a run into its initial task(s) and dispatch them.
async fn plan_run(
    state: &AppState,
    provider: &Provider,
    mode: ScanMode,
) -> anyhow::Result<ScanRunId> {
    let run_id =
        tankovault_db::repo::scans::create_run(&state.pool, Some(provider.id), mode).await?;
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
        tracing::info!(%run_id, provider = %provider.slug, ?mode, "planned scan run (initial task already existed)");
        return Ok(run_id);
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

    tracing::info!(%run_id, provider = %provider.slug, ?mode, "planned scan run");
    Ok(run_id)
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
    {
        tracing::info!("scheduler disabled");
        return;
    }
    let mut fast = interval_or_never(cfg.fast_interval_secs);
    let mut full = interval_or_never(cfg.full_interval_secs);
    let mut merge = interval_or_never(cfg.merge_sweep_interval_secs);

    loop {
        tokio::select! {
            () = shutdown.cancelled() => {
                tracing::info!("scheduler stopping");
                return;
            }
            () = tick(&mut fast) => maybe_sweep(&state, &leadership, ScanMode::Fast).await,
            () = tick(&mut full) => maybe_sweep(&state, &leadership, ScanMode::Full).await,
            () = tick(&mut merge) => maybe_merge_sweep(&state, &leadership).await,
        }
    }
}

/// Runs a duplicate sweep only when this replica holds leadership *and* automatic
/// merging is switched on.
///
/// Leadership matters more than for a scan sweep: two replicas merging the same pair
/// race on a destructive transaction, and the loser finds its series already gone.
async fn maybe_merge_sweep(state: &AppState, leadership: &leader::Leadership) {
    if !state.features.is_enabled(Feature::ScanningAutoMerge) {
        tracing::debug!("skipping duplicate sweep; automatic merging is switched off");
        return;
    }
    if !leadership.is_leader() {
        tracing::debug!("skipping duplicate sweep; not scheduler leader");
        return;
    }
    match dedupe::sweep(&state.pool, &state.matching, state.merge_budget, None).await {
        Ok(report) => tracing::info!(
            examined = report.pairs_examined,
            auto_merged = report.auto_merged,
            queued = report.queued,
            withdrawn = report.withdrawn,
            deferred = report.deferred,
            "duplicate sweep complete"
        ),
        Err(e) => tracing::warn!(error = %e, "duplicate sweep failed"),
    }
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

async fn sweep(state: &AppState, mode: ScanMode) {
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
        if let Err(e) = plan_run(state, provider, mode).await {
            tracing::warn!(provider = %provider.slug, error = %e, "scheduler: plan failed");
        }
    }
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
