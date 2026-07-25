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
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tankovault_bus::Bus;
use tankovault_contracts::{ScanTaskMessage, TaskKind};
use tankovault_db::PgPool;
use tankovault_domain::{Provider, ProviderId, ScanMode, ScanRunId};
use tankovault_service::health::PostgresCheck;
use tankovault_service::{Health, HttpStack, MetricsRegistry, RateLimiter, RouteClassifier};

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
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cfg: Config = tankovault_config::load()?;
    tankovault_service::init_tracing(&cfg.telemetry)?;
    let metrics = MetricsRegistry::install(&cfg.metrics)?;
    let shutdown = tankovault_service::install_shutdown();

    let pool = tankovault_db::connect(
        &cfg.database.url,
        cfg.database.max_connections,
        cfg.database.acquire_timeout_secs,
    )
    .await?;

    let bus = Bus::connect(&cfg.nats.url).await?;
    bus.ensure_streams().await?;

    let state = AppState {
        pool: pool.clone(),
        bus: bus.clone(),
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
    tokio::spawn(async move {
        if let Err(e) = aggregator::run(agg_pool, agg_bus).await {
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

#[derive(Debug, Serialize)]
struct TriggerResponse {
    run_ids: Vec<ScanRunId>,
}

async fn trigger_scan(
    State(state): State<AppState>,
    Json(req): Json<TriggerRequest>,
) -> Result<Json<TriggerResponse>, (StatusCode, String)> {
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
    Ok(Json(TriggerResponse { run_ids }))
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

/// Run a sweep only when this replica currently holds scheduler leadership.
async fn maybe_sweep(state: &AppState, leadership: &leader::Leadership, mode: ScanMode) {
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

fn internal<E: std::fmt::Display>(e: E) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}
