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
use tankovault_solver::ChallengeSolver;

#[derive(Debug, Deserialize)]
struct Config {
    database: DatabaseConfig,
    nats: NatsConfig,
    telemetry: TelemetryConfig,
    #[serde(default)]
    worker: WorkerConfig,
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
    tankovault_observability::init_tracing(&cfg.telemetry)?;

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
        [cmd, slug, mode] if cmd == "scan" => run_inline(&engine, slug, mode).await,
        [] => run_consumer(&engine).await,
        _ => {
            eprintln!("usage: worker [scan <provider_slug> <full|fast>]");
            std::process::exit(2);
        }
    }
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
async fn run_consumer(engine: &Engine) -> anyhow::Result<()> {
    let bus = engine
        .bus
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("consumer mode requires NATS, which is unavailable"))?;

    let consumer = bus.task_consumer().await?;
    let mut messages = consumer.messages().await?;
    tracing::info!(worker_id = %engine.worker_id, "worker consuming scan tasks");

    while let Some(next) = messages.next().await {
        let msg = match next {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(error = %e, "error pulling message");
                continue;
            }
        };

        match serde_json::from_slice::<ScanTaskMessage>(&msg.payload) {
            Ok(task) => {
                if let Err(e) = handle_task(engine, &task).await {
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
