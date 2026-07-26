//! # worker service
//!
//! Executes scan tasks. Two entry modes:
//! - `worker scan <provider_slug> <full|fast>` — a one-shot inline scan (no broker),
//!   the Phase-0 deliverable: full-scan a provider into Postgres, links-only, idempotent.
//! - `worker` (no args) — subscribe to the `JetStream` tasks stream (consumer group →
//!   horizontal scale) and process tasks until shutdown.

mod engine;

use engine::Engine;
use futures::StreamExt;
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;
use tankovault_bus::Bus;
use tankovault_config::{DatabaseConfig, NatsConfig, TelemetryConfig};
use tankovault_contracts::ScanTaskMessage;
use tankovault_fetch::{HttpChallengeSolver, InMemorySessionStore, SessionStore};
use tankovault_service::health::PostgresCheck;
use tankovault_service::{Health, HttpStack, MetricsRegistry};
use tankovault_solver::ChallengeSolver;

#[derive(Debug, Deserialize)]
struct Config {
    database: DatabaseConfig,
    nats: NatsConfig,
    telemetry: TelemetryConfig,
    #[serde(default)]
    worker: WorkerConfig,
    /// Ops listener binding. The worker has no HTTP contract of its own, but an
    /// orchestrator still needs somewhere to send liveness/readiness probes and a
    /// scrape target — previously it exposed neither, so a wedged worker was invisible.
    #[serde(default = "default_bind")]
    bind_addr: String,
    /// Edge hardening for the ops listener.
    #[serde(default)]
    security: tankovault_config::SecurityConfig,
    /// Prometheus metrics. Togglable; disabling installs no recorder.
    #[serde(default)]
    metrics: tankovault_config::MetricsConfig,
}

fn default_bind() -> String {
    "0.0.0.0:8085".to_owned()
}

#[derive(Debug, Deserialize)]
struct WorkerConfig {
    #[serde(default = "default_solver_endpoint")]
    challenge_solver_endpoint: String,
    #[serde(default = "default_max_pages")]
    max_catalog_pages: u32,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            challenge_solver_endpoint: default_solver_endpoint(),
            max_catalog_pages: default_max_pages(),
        }
    }
}

fn default_solver_endpoint() -> String {
    "http://challenge-solver:8090".to_owned()
}
fn default_max_pages() -> u32 {
    // Purely a runaway-paginator backstop (real termination is the adapter's `has_next`
    // marker) — some providers legitimately paginate into the thousands (e.g. kunmanga's
    // ~6866-page catalogue), so this must sit well above any real catalogue size.
    20_000
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

    // The broker is required for the consumer path but optional for a one-shot scan.
    let bus = match Bus::connect(&cfg.nats.url).await {
        Ok(bus) => {
            bus.ensure_streams().await?;
            Some(bus)
        }
        Err(e) => {
            tracing::warn!(error = %e, "NATS unavailable; broker features disabled");
            None
        }
    };

    let solver: Arc<dyn ChallengeSolver> = Arc::new(HttpChallengeSolver::new(
        cfg.worker.challenge_solver_endpoint.clone(),
        Duration::from_secs(90),
    ));
    let session_store: Arc<dyn SessionStore> = Arc::new(InMemorySessionStore::default());

    let engine = Engine {
        pool: pool.clone(),
        bus: bus.clone(),
        solver,
        session_store,
        worker_id: format!("worker-{}", uuid::Uuid::now_v7()),
        max_catalog_pages: cfg.worker.max_catalog_pages,
    };

    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        // A one-shot inline scan is a CLI invocation, not a served workload: no listener,
        // and it exits when the scan does.
        [cmd, slug, mode] if cmd == "scan" => run_inline(&engine, slug, mode).await,
        [] => {
            spawn_ops_listener(&cfg, &pool, bus.as_ref(), metrics, shutdown.clone());
            run_consumer(&engine, shutdown).await
        }
        _ => {
            eprintln!("usage: worker [scan <provider_slug> <full|fast>]");
            std::process::exit(2);
        }
    }
}

/// Serve liveness, readiness and the metrics scrape alongside the consumer loop.
///
/// Readiness reports both dependencies. A worker without NATS has nothing to consume and a
/// worker without Postgres cannot record what it consumed, so neither should be counted as
/// a healthy replica.
fn spawn_ops_listener(
    cfg: &Config,
    pool: &tankovault_db::PgPool,
    bus: Option<&Bus>,
    metrics: MetricsRegistry,
    shutdown: tokio_util::sync::CancellationToken,
) {
    let ready_pool = pool.clone();
    let ready_bus = bus.cloned();
    let health = Health::builder()
        .check(PostgresCheck::new(ready_pool))
        .check_fn("nats", move || {
            let bus = ready_bus.clone();
            async move {
                match bus {
                    Some(bus) => bus.ping().await.map_err(|e| e.to_string()),
                    None => Err("not connected".to_owned()),
                }
            }
        })
        .build();

    let app = HttpStack::new(&cfg.security, metrics.clone())
        .apply(axum::Router::new())
        .merge(tankovault_service::ops_router(health, metrics));

    let bind = cfg.bind_addr.clone();
    tokio::spawn(async move {
        if let Err(e) = tankovault_service::serve(&bind, app, shutdown).await {
            tracing::error!(error = %e, "worker ops listener stopped");
        }
    });
}

/// One-shot inline scan (no broker).
async fn run_inline(engine: &Engine, slug: &str, mode: &str) -> anyhow::Result<()> {
    let provider = tankovault_db::repo::providers::get_by_slug(&engine.pool, slug).await?;
    tracing::info!(%slug, %mode, "starting inline scan");

    let summary = match mode {
        "full" => engine.run_full_scan_inline(&provider).await?,
        "fast" => engine.run_fast_scan_inline(&provider).await?,
        other => anyhow::bail!("unknown mode {other:?}; expected full|fast"),
    };

    tracing::info!(
        series_seen = summary.series_seen,
        series_failed = summary.series_failed,
        new_chapters = summary.new_chapters,
        "inline scan complete"
    );
    println!(
        "scan complete: {} series seen, {} failed, {} new chapters",
        summary.series_seen, summary.series_failed, summary.new_chapters
    );
    Ok(())
}

/// `JetStream` consumer loop.
///
/// Stops between tasks on shutdown rather than being severed mid-scan. A task killed
/// part-way through stays claimed until its visibility timeout expires, so draining
/// cleanly is what keeps a rolling restart from stalling every in-flight run.
async fn run_consumer(
    engine: &Engine,
    shutdown: tokio_util::sync::CancellationToken,
) -> anyhow::Result<()> {
    let bus = engine
        .bus
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("consumer mode requires NATS, which is unavailable"))?;

    let consumer = bus.task_consumer().await?;
    let mut messages = consumer.messages().await?;
    tracing::info!(worker_id = %engine.worker_id, "worker consuming scan tasks");

    loop {
        let next = tokio::select! {
            () = shutdown.cancelled() => {
                tracing::info!(worker_id = %engine.worker_id, "worker stopping");
                return Ok(());
            }
            next = messages.next() => match next {
                Some(next) => next,
                None => break,
            },
        };
        let msg = match next {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(error = %e, "error pulling message");
                continue;
            }
        };

        match serde_json::from_slice::<ScanTaskMessage>(&msg.payload) {
            Ok(task) => {
                // Wrapped here rather than inside the engine: ack lifetime belongs to
                // whoever owns the message, and doing it at the loop means *every* task
                // kind is covered — a 20k-entry catalogue page today, whatever runs long
                // tomorrow — instead of each slow path having to remember.
                let result = tankovault_bus::with_ack_heartbeat(
                    &msg,
                    tankovault_bus::TASK_ACK_HEARTBEAT,
                    handle_task(engine, &task),
                )
                .await;
                if let Err(e) = result {
                    tracing::warn!(task_id = %task.task_id, error = %e, "task failed");
                    let _ = tankovault_db::repo::scans::fail_task(
                        &engine.pool,
                        task.task_id,
                        &e.to_string(),
                    )
                    .await;
                }
                // Republish progress after the task settles (done or failed) so the
                // control-plane aggregator can finalise the run and the console can
                // relay live progress over NATS (design §12).
                engine.report_progress(task.run_id).await;
            }
            Err(e) => tracing::warn!(error = %e, "undecodable task message; dropping"),
        }
        if let Err(e) = msg.ack().await {
            tracing::warn!(error = %e, "failed to ack message");
        }
    }
    Ok(())
}

async fn handle_task(engine: &Engine, task: &ScanTaskMessage) -> anyhow::Result<()> {
    let provider = tankovault_db::repo::providers::get(&engine.pool, task.provider_id).await?;
    tankovault_db::repo::scans::claim_task(&engine.pool, task.task_id, &engine.worker_id).await?;
    // A `CatalogPage` task fans out its children (and bumps the run total) before this
    // returns, so completing it here cannot finalise the run prematurely.
    engine.dispatch_task(&provider, task).await?;
    tankovault_db::repo::scans::complete_task(&engine.pool, task.task_id).await?;
    Ok(())
}
