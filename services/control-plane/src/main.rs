//! # control-plane service
//!
//! Schedules and plans scans; workers execute. Responsibilities (design §12):
//! - **Planner**: expand a run into `scan_tasks`, write them, and publish to the
//!   provider's `JetStream` subject.
//! - **Scheduler**: periodically trigger fast scans of active providers (and full scans
//!   on a slower cadence). A richer per-provider cron (`tokio-cron-scheduler`) + Redis
//!   leader election are documented follow-ups.
//! - **Trigger endpoint**: `POST /internal/scans` called by the API's "Scan now".

mod aggregator;
mod leader;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;
use std::time::Duration;
use tankovault_bus::Bus;
use tankovault_contracts::admin::ScanTriggeredView;
use tankovault_contracts::{ScanTaskMessage, TaskKind};
use tankovault_db::PgPool;
use tankovault_domain::{Feature, Provider, ProviderId, ScanMode, ScanRunId};
use tankovault_service::health::PostgresCheck;
use tankovault_service::problem::Problem;
use tankovault_service::{
    FeatureGate, Health, HttpStack, MetricsRegistry, PostgresFlagSource, RateLimiter,
    RouteClassifier,
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
}

fn default_bind() -> String {
    "0.0.0.0:8081".to_owned()
}

#[derive(Debug, Deserialize)]
struct SchedulerConfig {
    /// Seconds between fast-scan sweeps of all active providers. 0 disables.
    #[serde(default = "default_fast_interval")]
    fast_interval_secs: u64,
    /// Seconds between full-scan sweeps. 0 disables (full scans are usually on demand).
    #[serde(default)]
    full_interval_secs: u64,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            fast_interval_secs: default_fast_interval(),
            full_interval_secs: 0,
        }
    }
}

fn default_fast_interval() -> u64 {
    300
}

#[derive(Clone)]
struct AppState {
    pool: PgPool,
    bus: Bus,
    /// The operator's runtime switches. Consulted at the top of each scheduler sweep rather
    /// than at boot: switching the scheduler off during an incident has to take effect without
    /// a redeploy, which is the whole point of a flag as opposed to a config toggle.
    features: FeatureGate,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Before config, telemetry or anything else: this process may have been invoked by
    // Docker's HEALTHCHECK rather than as the service. `scratch` images have no shell and no
    // wget, so the binary probing itself is the only probe available. See
    // `tankovault_service::healthcheck`.
    if tankovault_service::healthcheck::requested() {
        let cfg: Config = tankovault_config::load()?;
        tankovault_service::run_healthcheck_and_exit(&cfg.bind_addr);
    }

    let cfg: Config = tankovault_config::load()?;
    tankovault_service::init_tracing(&cfg.telemetry)?;
    let metrics = MetricsRegistry::install(&cfg.metrics)?;
    // Resolved before anything binds: a service in this tier that starts without a token
    // silently downgrades to the unauthenticated behaviour the token exists to remove, so
    // the production profile refuses to boot rather than serving privileged routes openly.
    let internal_token = tankovault_service::internal_auth::resolve(&cfg.internal)?;
    let shutdown = tankovault_service::install_shutdown();

    let pool = tankovault_db::connect(
        &cfg.database.url,
        cfg.database.max_connections,
        cfg.database.acquire_timeout_secs,
    )
    .await?;

    let bus = Bus::connect(&cfg.nats.url).await?;
    bus.ensure_streams().await?;

    // Loaded before the scheduler starts, so the first sweep after a restart already respects
    // the operator's stored decisions rather than briefly running against the defaults.
    let features = FeatureGate::new(std::sync::Arc::new(PostgresFlagSource::new(pool.clone())));
    features
        .spawn_refresh(cfg.features.refresh_interval(), shutdown.clone())
        .await;

    let state = AppState {
        pool: pool.clone(),
        bus: bus.clone(),
        features,
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
    tokio::spawn(async move {
        run_scheduler(sched_state, cfg.scheduler, sched_leadership, sched_shutdown).await;
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

    tankovault_service::spawn_metrics_server(metrics.clone(), shutdown.clone());

    let limiter = RateLimiter::from_config(&cfg.rate_limit, RouteClassifier::new(), None);
    let app = HttpStack::new(&cfg.security, metrics.clone())
        .with_rate_limit(limiter)
        .with_internal_auth(internal_token)
        .apply(
            Router::new()
                .route("/internal/scans", post(trigger_scan))
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

// The response shape is `tankovault_contracts::admin::ScanTriggeredView`, not a private struct
// here: `services/api` republishes this body verbatim on `/v1/admin/scans`, and while the
// definition lived in this binary the republisher could declare nothing more specific than
// `serde_json::Value` (ARCH-10). Both ends name the same type now.

async fn trigger_scan(
    State(state): State<AppState>,
    Json(req): Json<TriggerRequest>,
) -> Result<Json<ScanTriggeredView>, Problem> {
    // The API refuses a full scan on the same flag before it ever reaches here. Repeating the
    // check is not redundant: this endpoint is reachable by anything on the internal network,
    // and a switch an operator has thrown should hold at the component that does the work, not
    // only at the one that happens to be in front of it today.
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
/// Exits on `shutdown` rather than being killed mid-sweep: a sweep that is severed
/// part-way through leaves planned-but-unpublished runs behind, which the aggregator then
/// waits on forever.
async fn run_scheduler(
    state: AppState,
    cfg: SchedulerConfig,
    leadership: leader::Leadership,
    shutdown: tokio_util::sync::CancellationToken,
) {
    if cfg.fast_interval_secs == 0 && cfg.full_interval_secs == 0 {
        tracing::info!("scheduler disabled");
        return;
    }
    let mut fast = interval_or_never(cfg.fast_interval_secs);
    let mut full = interval_or_never(cfg.full_interval_secs);

    loop {
        tokio::select! {
            () = shutdown.cancelled() => {
                tracing::info!("scheduler stopping");
                return;
            }
            () = tick(&mut fast) => maybe_sweep(&state, &leadership, ScanMode::Fast).await,
            () = tick(&mut full) => maybe_sweep(&state, &leadership, ScanMode::Full).await,
        }
    }
}

/// Run a sweep only when this replica currently holds scheduler leadership *and* the operator
/// has left scheduled scanning switched on.
///
/// The flag is checked here, per sweep, rather than when the loop is built: an operator
/// switching `scanning.scheduler` off — typically because a provider is complaining about
/// traffic — needs the next sweep to skip, not a redeploy. The loop itself keeps running so it
/// resumes on its own when the flag comes back.
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

/// Log the cause and answer with an opaque `500` [`Problem`].
///
/// This used to be `(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())`, which put the raw
/// `Display` of a database or bus error on the wire — connection strings and SQL included — and
/// left no trace in the log. It is now the other way around, which is the only correct way
/// around, and it emits the same RFC 9457 body as every other service (ARCH-12).
fn internal<E: std::fmt::Display>(e: E) -> Problem {
    tracing::error!(error = %e, "control-plane request failed");
    Problem::internal()
}
