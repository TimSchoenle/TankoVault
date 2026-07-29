//! # worker service
//!
//! Executes scan tasks. Two entry modes:
//! - `worker scan <provider_slug> <full|fast>` — a one-shot inline scan (no broker),
//!   the Phase-0 deliverable: full-scan a provider into Postgres, links-only, idempotent.
//! - `worker` (no args) — subscribe to the `JetStream` tasks stream (consumer group →
//!   horizontal scale) and process tasks until shutdown.

mod engine;
mod queue;

use engine::Engine;
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;
use tankovault_bus::Bus;
use tankovault_config::{DatabaseConfig, NatsConfig, TelemetryConfig};
use tankovault_contracts::{ScanTaskMessage, TaskKind};
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
    /// Shared secret presented to `challenge-solver`. The worker exposes no contract of its
    /// own, so this is outbound-only.
    #[serde(default)]
    internal: tankovault_config::InternalAuthConfig,
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
    /// How often the round-robin queue re-reads the provider list, in seconds.
    ///
    /// This is the delay before a provider added (or renamed) while the pool is running has
    /// its task lane opened and starts being scanned — a one-off at provider onboarding, so
    /// a minute costs nothing and keeps the query off the hot path.
    #[serde(default = "default_provider_refresh_secs")]
    provider_refresh_secs: u64,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            challenge_solver_endpoint: default_solver_endpoint(),
            max_catalog_pages: default_max_pages(),
            provider_refresh_secs: default_provider_refresh_secs(),
        }
    }
}

fn default_solver_endpoint() -> String {
    "http://challenge-solver:8090".to_owned()
}
fn default_provider_refresh_secs() -> u64 {
    60
}
fn default_max_pages() -> u32 {
    // Purely a runaway-paginator backstop (real termination is the adapter's `has_next`
    // marker) — some providers legitimately paginate into the thousands (e.g. kunmanga's
    // ~6866-page catalogue), so this must sit well above any real catalogue size.
    20_000
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
    let internal_token = tankovault_service::internal_auth::resolve(&cfg.internal)?;
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
        internal_token.as_ref().map(|t| t.expose().to_owned()),
    ));
    let session_store: Arc<dyn SessionStore> = Arc::new(InMemorySessionStore::default());

    let engine = Engine::new(
        pool.clone(),
        bus.clone(),
        solver,
        session_store,
        format!("worker-{}", uuid::Uuid::now_v7()),
        cfg.worker.max_catalog_pages,
    );

    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        // A one-shot inline scan is a CLI invocation, not a served workload: no listener,
        // and it exits when the scan does.
        [cmd, slug, mode] if cmd == "scan" => run_inline(&engine, slug, mode).await,
        [] => {
            // Serve the metrics scrape on its own port when configured, keeping it off the
            // request-facing ops listener.
            tankovault_service::spawn_metrics_server(metrics.clone(), shutdown.clone());
            spawn_ops_listener(&cfg, &pool, bus.as_ref(), metrics, shutdown.clone());
            run_consumer(&engine, &cfg.worker, shutdown).await
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
/// Tasks are taken from the round-robin [`queue::FairQueue`] rather than straight off a
/// wildcard consumer, so which provider runs next is a scheduling decision instead of
/// whatever the stream holds at its head. Everything below the pull is unchanged: one task
/// at a time, per-provider rate limits still governing the fetch stack.
///
/// Stops between tasks on shutdown rather than being severed mid-scan. A task killed
/// part-way through stays claimed until its visibility timeout expires, so draining
/// cleanly is what keeps a rolling restart from stalling every in-flight run.
async fn run_consumer(
    engine: &Engine,
    cfg: &WorkerConfig,
    shutdown: tokio_util::sync::CancellationToken,
) -> anyhow::Result<()> {
    let bus = engine
        .bus
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("consumer mode requires NATS, which is unavailable"))?;

    let mut queue = queue::FairQueue::open(
        bus.clone(),
        engine.pool.clone(),
        Duration::from_secs(cfg.provider_refresh_secs),
    )
    .await?;
    tracing::info!(worker_id = %engine.worker_id, "worker consuming scan tasks");

    while let Some(msg) = queue.next_task(&shutdown).await {
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
                    let deliveries = tankovault_bus::delivery_count(&msg);
                    if is_retryable(&e) && deliveries < MAX_TASK_DELIVERIES {
                        let delay = retry_delay(deliveries);
                        log_task_failure(
                            &task,
                            deliveries,
                            &e,
                            &format!(
                                "requeued; delivery {} of {MAX_TASK_DELIVERIES} follows in {}s",
                                deliveries + 1,
                                delay.as_secs()
                            ),
                        );
                        if let Err(e) = tankovault_bus::retry_later(&msg, delay).await {
                            tracing::warn!(
                                task_id = %task.task_id,
                                error = %e,
                                "could not requeue task; it will be redelivered when the ack \
                                 deadline lapses"
                            );
                        }
                        // Left unsettled and uncounted: the run stays open for it, and the
                        // idempotent writes make the re-run a no-op for whatever it did do.
                        continue;
                    }
                    let next = if is_retryable(&e) {
                        format!(
                            "gave up after {MAX_TASK_DELIVERIES} deliveries; recorded as failed \
                             and the run continues without it"
                        )
                    } else {
                        "will fail identically on replay; recorded as failed and the run \
                         continues without it"
                            .to_owned()
                    };
                    log_task_failure(&task, deliveries, &e, &next);
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
    tracing::info!(worker_id = %engine.worker_id, "worker stopping");
    Ok(())
}

/// Deliveries a scan task gets before its failure is treated as final.
///
/// Sized against what actually recovers between attempts — a challenge solve, a provider's
/// rate-limit window, a solver restart. Past that, "transient" is not a useful description of
/// the failure, and further attempts only delay the run's completion.
const MAX_TASK_DELIVERIES: u64 = 3;

/// Whether `err` is worth another delivery.
///
/// The adapter layer owns this judgement ([`AdapterError::is_transient`]) because it is the
/// layer that knows the difference between a provider that blocked us and a page whose markup
/// changed. Anything else — a DB write, a broker publish, a malformed task — fails the task:
/// a worker that cannot reach Postgres has a problem no redelivery fixes.
fn is_retryable(err: &anyhow::Error) -> bool {
    err.downcast_ref::<tankovault_adapters::AdapterError>()
        .is_some_and(tankovault_adapters::AdapterError::is_transient)
}

/// Backoff before a requeued task is redelivered.
///
/// Minutes rather than seconds, deliberately: the failures worth retrying are provider-side,
/// and retrying into a rate limit or a bot-management block faster than it clears is how a
/// scan becomes the thing that keeps it blocked.
fn retry_delay(deliveries: u64) -> Duration {
    match deliveries {
        0 | 1 => Duration::from_secs(60),
        2 => Duration::from_secs(300),
        _ => Duration::from_secs(900),
    }
}

/// The task kind as it is spelled on the wire and in `scan_tasks.kind`, so a console line
/// and a row in the table can be correlated without a translation step.
fn task_kind_name(kind: TaskKind) -> &'static str {
    match kind {
        TaskKind::CatalogPage => "catalog_page",
        TaskKind::Series => "series",
        TaskKind::LatestFeed => "latest_feed",
    }
}

/// What a task was pointed at, in the terms the provider uses.
///
/// A task id identifies a row; it does not tell whoever is watching the console *which page*
/// of the catalogue or *which series* just failed, which is the only part they can act on.
fn describe_target(task: &ScanTaskMessage) -> String {
    match task.kind {
        TaskKind::CatalogPage => format!(
            "catalog page {}",
            task.target
                .get("page")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(1)
        ),
        TaskKind::Series => task
            .target
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("<no path>")
            .to_owned(),
        TaskKind::LatestFeed => "latest-updates feed".to_owned(),
    }
}

/// Report a failed task, and what becomes of it.
///
/// A scan is watched from the console, so one failure is one line answering three questions:
/// what was being scanned, what the provider did about it, and whether anything will try
/// again. `next` carries the third — the retry decision lives at the call site, because only
/// there are the delivery count and the backoff known.
fn log_task_failure(task: &ScanTaskMessage, deliveries: u64, err: &anyhow::Error, next: &str) {
    tracing::warn!(
        provider = %task.provider_slug,
        scan = %task.mode,
        task = task_kind_name(task.kind),
        target = %describe_target(task),
        run_id = %task.run_id,
        task_id = %task.task_id,
        delivery = deliveries,
        max_deliveries = MAX_TASK_DELIVERIES,
        error = %err,
        next = %next,
        "scan task failed"
    );
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

#[cfg(test)]
mod tests {
    use super::*;
    use tankovault_domain::{ProviderId, ScanMode, ScanRunId, ScanTaskId};

    fn task(kind: TaskKind, target: serde_json::Value) -> ScanTaskMessage {
        ScanTaskMessage {
            task_id: ScanTaskId::from_uuid(uuid::Uuid::nil()),
            run_id: ScanRunId::from_uuid(uuid::Uuid::nil()),
            provider_id: ProviderId::from_uuid(uuid::Uuid::nil()),
            provider_slug: "kunmanga".to_owned(),
            mode: ScanMode::Full,
            kind,
            target,
            traceparent: None,
        }
    }

    #[test]
    fn a_series_target_is_the_path_that_was_scanned() {
        let msg = task(
            TaskKind::Series,
            serde_json::json!({ "path": "/manga/berserk/" }),
        );
        assert_eq!(describe_target(&msg), "/manga/berserk/");
    }

    #[test]
    fn a_catalog_target_is_the_page_number() {
        let msg = task(TaskKind::CatalogPage, serde_json::json!({ "page": 7 }));
        assert_eq!(describe_target(&msg), "catalog page 7");
    }

    #[test]
    fn a_malformed_target_still_logs() {
        let msg = task(TaskKind::Series, serde_json::json!({}));
        assert_eq!(describe_target(&msg), "<no path>");
        let msg = task(TaskKind::LatestFeed, serde_json::Value::Null);
        assert_eq!(describe_target(&msg), "latest-updates feed");
    }

    /// A provider-side refusal is transient; a broken page or a database fault is not.
    ///
    /// This is the whole retry policy in one predicate, and getting it backwards is silent
    /// either way: treating a permanent failure as transient burns three deliveries and 26
    /// minutes per task against a provider that will never answer, while treating a throttle
    /// as permanent drops real chapters on the floor after one attempt.
    #[test]
    fn only_a_provider_side_failure_is_worth_another_delivery() {
        use tankovault_adapters::AdapterError;

        let throttled = anyhow::Error::from(AdapterError::Throttled {
            url: "https://provider.test/manga/x/".to_owned(),
        });
        assert!(is_retryable(&throttled), "a throttle clears on its own");

        // A parse failure reproduces exactly on replay: the markup does not change between
        // deliveries, so retrying it only delays the run.
        let parse = anyhow::Error::from(AdapterError::Parse("selector matched nothing".to_owned()));
        assert!(!is_retryable(&parse));

        // Anything that is not an adapter error at all — a database write, a broker publish —
        // is the worker's own problem and no redelivery fixes it.
        assert!(!is_retryable(&anyhow::anyhow!("connection pool exhausted")));
    }

    /// Backoff grows and then stops growing.
    ///
    /// The growth matters because the failures being retried are provider-side: retrying into
    /// a rate-limit window faster than it clears is how a scan becomes the reason it stays
    /// blocked. The cap matters because an unbounded delay would hold a run open indefinitely.
    #[test]
    fn retry_delay_grows_monotonically_and_is_capped() {
        let delays: Vec<Duration> = (0..8).map(retry_delay).collect();
        for pair in delays.windows(2) {
            assert!(
                pair[1] >= pair[0],
                "backoff went backwards: {:?} then {:?}",
                pair[0],
                pair[1]
            );
        }
        let cap = *delays.last().expect("delays is non-empty");
        assert_eq!(
            cap,
            retry_delay(u64::MAX),
            "the backoff must be capped, not open-ended"
        );
        assert!(
            delays[0] >= Duration::from_secs(60),
            "the first backoff must outlast a provider's rate-limit window, got {:?}",
            delays[0]
        );
    }

    /// The delivery ceiling bounds how long one failing task can hold a scan run open.
    ///
    /// A run finalises only once every task settles, so the worst case here is the worst case
    /// for the run's completion and for everything downstream of it. Raising
    /// `MAX_TASK_DELIVERIES` or the backoff cap without noticing that second effect is the
    /// regression this pins, and it states the bound in wall-clock terms so the trade-off is
    /// legible rather than implied by two constants sitting far apart in the file.
    #[test]
    fn the_delivery_ceiling_bounds_how_long_a_task_can_hold_a_run_open() {
        const {
            assert!(
                MAX_TASK_DELIVERIES > 1,
                "a ceiling of one means a transient provider failure is never retried at all"
            );
        }

        let worst_case: Duration = (0..MAX_TASK_DELIVERIES).map(retry_delay).sum();
        assert!(
            worst_case <= Duration::from_secs(30 * 60),
            "a single task can now delay its run by {worst_case:?}, past the half hour the \
             ceiling was sized for"
        );
    }
}
