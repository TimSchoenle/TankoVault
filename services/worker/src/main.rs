//! # worker service
//!
//! Executes scan tasks. Two entry modes:
//! - `worker scan <provider_slug> <full|fast>` — a one-shot inline scan (no broker),
//!   the Phase-0 deliverable: full-scan a provider into Postgres, links-only, idempotent.
//! - `worker` (no args) — subscribe to the `JetStream` tasks stream (consumer group →
//!   horizontal scale) and process tasks until shutdown.

mod dryrun;
mod engine;
mod queue;

use engine::{Engine, EngineSettings};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tankovault_bus::Bus;
use tankovault_config::{DatabaseConfig, NatsConfig, TelemetryConfig};
use tankovault_contracts::{ScanTaskMessage, TaskKind};
use tankovault_fetch::{HttpChallengeSolver, InMemorySessionStore, SessionStore};
use tankovault_service::health::PostgresCheck;
use tankovault_service::{CancellationToken, Health, HttpStack, MetricsRegistry};
use tankovault_solver::ChallengeSolver;
use tokio::task::JoinSet;

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
    /// The confidence policy for canonicalising a scanned series onto an existing one. Shared
    /// with external sync so the two paths cannot disagree about whether two series are the
    /// same (ARCH-16).
    #[serde(default)]
    matching: tankovault_config::MatchingConfig,
    /// Which source owns each metadata field. Shared with external sync, which writes the same
    /// columns: a scan that ignored it overwrote every enriched description on its next pass.
    #[serde(default)]
    metadata: tankovault_config::MetadataPriorityConfig,
    /// Which scraped chapter numbers a scan refuses to index. Sources publish stray entries
    /// numbered from dates, years and title text; left in, one of them becomes the series'
    /// latest chapter.
    #[serde(default)]
    chapter_outliers: tankovault_config::ChapterOutlierConfig,
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
    /// How many providers this worker scans at once.
    ///
    /// A worker runs at most one task per provider, so this is both the task concurrency and
    /// the count of distinct providers in flight. Crawl politeness is unaffected: `rps` and
    /// `concurrency` are enforced by a fetch stack cached per provider, which every task for
    /// that provider shares. The database pool is *not* — size `database.max_connections`
    /// for this many concurrent scans, or tasks queue on `acquire` and report as timeouts
    /// that read like a database fault.
    #[serde(default = "default_max_concurrent_providers")]
    max_concurrent_providers: usize,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            challenge_solver_endpoint: default_solver_endpoint(),
            max_catalog_pages: default_max_pages(),
            provider_refresh_secs: default_provider_refresh_secs(),
            max_concurrent_providers: default_max_concurrent_providers(),
        }
    }
}

impl WorkerConfig {
    /// The concurrency limit, floored at one.
    ///
    /// Zero does not disable scanning, it deadlocks: the loop would never be under the limit,
    /// so it would never claim a task and never spawn one to get back under it. The worker
    /// would sit idle against a full queue with nothing in the logs to say why. `active` on
    /// the provider is how scanning is turned off.
    fn max_concurrent_providers(&self) -> usize {
        self.max_concurrent_providers.max(1)
    }
}

fn default_solver_endpoint() -> String {
    "http://challenge-solver:8090".to_owned()
}
fn default_provider_refresh_secs() -> u64 {
    60
}
fn default_max_concurrent_providers() -> usize {
    // Providers, not requests: each still crawls under its own `rps`/`concurrency` budget.
    // Four is sized to keep the blocking pool (one parse per in-flight task) and the database
    // pool comfortable on the shipped container, not to any provider-side limit.
    4
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

    let boot = tankovault_config::load_watched::<Config>()?;
    // Both are process-global and installed once, which is why `telemetry.*` and `metrics.*`
    // are the two blocks a configuration reload cannot apply.
    tankovault_service::init_tracing(&boot.value.telemetry)?;
    let metrics =
        MetricsRegistry::install(&boot.value.metrics, &boot.value.telemetry.service_name)?;
    let shutdown = tankovault_service::install_shutdown();

    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        // A one-shot inline scan is a CLI invocation, not a served workload: no listener, no
        // reload supervisor, and it exits when the scan does.
        [cmd, slug, mode] if cmd == "scan" => {
            let built = build(&boot.value).await?;
            run_inline(&built.engine, slug, mode).await
        }
        [] => {
            // Serve the metrics scrape on its own port when configured, keeping it off the
            // request-facing ops listener. Outside the reloadable runtime so a reload does
            // not rebind it.
            tankovault_service::spawn_metrics_server(metrics.clone(), shutdown.clone());
            tankovault_service::run_reloading(boot, &shutdown, |cfg, generation| {
                serve_once(cfg, metrics.clone(), generation)
            })
            .await
        }
        _ => {
            eprintln!("usage: worker [scan <provider_slug> <full|fast>]");
            std::process::exit(2);
        }
    }
}

/// Everything a scan needs, however the process was invoked.
struct Built {
    engine: Engine,
    pool: tankovault_db::PgPool,
    bus: Option<Bus>,
    internal_auth: tankovault_config::ResolvedInternalAuth,
}

/// Connect the dependencies and assemble the scan engine.
///
/// Shared by the one-shot CLI path and the served path so the two cannot drift into scanning
/// with differently configured engines.
async fn build(cfg: &Config) -> anyhow::Result<Built> {
    let internal_auth = tankovault_service::internal_auth::resolve(&cfg.internal)?;
    tankovault_service::internal_auth::check_upstream_scheme(
        internal_auth.mode,
        "challenge-solver",
        &cfg.worker.challenge_solver_endpoint,
    )?;

    let pool = tankovault_db::connect(
        &cfg.database.url,
        cfg.database.max_connections,
        cfg.database.acquire_timeout_secs,
    )
    .await?;

    // The broker is required for the consumer path but optional for a one-shot scan.
    let bus = match Bus::connect(&cfg.nats.url, internal_auth.tls.as_ref()).await {
        Ok(bus) => {
            bus.ensure_streams().await?;
            Some(bus)
        }
        Err(e) => {
            tracing::warn!(error = %e, "NATS unavailable; broker features disabled");
            None
        }
    };

    // The worker's *outbound* credential, distinct from what it accepts inbound: the solver
    // recognises `worker` by this and nothing else in the tier presents it.
    let solver: Arc<dyn ChallengeSolver> = Arc::new(match internal_auth.tls.as_ref() {
        Some(paths) => HttpChallengeSolver::with_mtls(
            cfg.worker.challenge_solver_endpoint.clone(),
            Duration::from_secs(90),
            &tankovault_service::client_material(paths)?,
        ),
        None => HttpChallengeSolver::new(
            cfg.worker.challenge_solver_endpoint.clone(),
            Duration::from_secs(90),
            internal_auth.caller.as_ref().and_then(|c| c.token.clone()),
        ),
    });
    let session_store: Arc<dyn SessionStore> = Arc::new(InMemorySessionStore::default());

    let engine = Engine::new(
        pool.clone(),
        bus.clone(),
        solver,
        session_store,
        format!("worker-{}", uuid::Uuid::now_v7()),
        EngineSettings {
            max_catalog_pages: cfg.worker.max_catalog_pages,
            matching: cfg.matching.clone(),
            metadata_priority: cfg.metadata.priority.clone(),
            tag_blocklist: cfg.metadata.tag_blocklist(),
            adult_tags: cfg.metadata.tags.adult_tags(),
            outliers: cfg.chapter_outliers.policy(),
        },
    );

    Ok(Built {
        engine,
        pool,
        bus,
        internal_auth,
    })
}

/// Build and run everything a configuration change rebuilds: the pool, the bus connection, the
/// scan engine, the ops listener and the consumer loop.
///
/// Returns when `shutdown` is cancelled — by the OS signal, or by the supervisor because the
/// configuration changed and this runtime is being replaced.
async fn serve_once(
    cfg: Arc<Config>,
    metrics: MetricsRegistry,
    shutdown: CancellationToken,
) -> anyhow::Result<()> {
    let built = build(&cfg).await?;
    tankovault_service::metrics::spawn_pool_sampler(built.pool.clone(), shutdown.clone());
    let engine = Arc::new(built.engine);
    spawn_ops_listener(
        &cfg,
        &built.pool,
        built.bus.as_ref(),
        metrics,
        Arc::clone(&engine),
        built.internal_auth.clone(),
        shutdown.clone(),
    );
    run_consumer(&engine, &cfg.worker, shutdown).await
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
    engine: Arc<Engine>,
    internal_auth: tankovault_config::ResolvedInternalAuth,
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

    // The dry-run route goes *inside* `apply`, so it is behind the internal-token gate;
    // `ops_router` is merged outside it, so an orchestrator still probes without the secret.
    let app = HttpStack::new(&cfg.security, metrics.clone())
        .with_internal_auth(Some(tankovault_service::InternalAuth::new(
            &internal_auth,
            tankovault_service::RouteTable(dryrun::INTERNAL_ROUTES),
        )))
        .apply(dryrun::router(engine))
        .merge(tankovault_service::ops_router(health, metrics));

    let bind = cfg.bind_addr.clone();
    tokio::spawn(async move {
        if let Err(e) =
            tankovault_service::serve_internal(&bind, app, &internal_auth, shutdown).await
        {
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
/// whatever the stream holds at its head.
///
/// Several tasks run at once, but **at most one per provider** — the ceiling is
/// `max_concurrent_providers` distinct providers in flight. That cap is doing more work than
/// it looks: a `scan_runs` row belongs to exactly one provider, so one task per provider
/// keeps every task of a run serialised, and the run's task accounting and the `CatalogPage`
/// fan-out ordering stay correct without any locking. Raising it to more than one task per
/// provider would forfeit that, and buy almost nothing — tasks for one provider share a
/// cached fetch stack, so they would queue on the same semaphore anyway.
///
/// Stops claiming on shutdown and then drains what is already running, rather than severing
/// it. A task killed part-way through stays claimed until its visibility timeout expires, so
/// draining cleanly is what keeps a rolling restart from stalling every in-flight run.
async fn run_consumer(
    engine: &Arc<Engine>,
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
    let limit = cfg.max_concurrent_providers();
    tracing::info!(
        worker_id = %engine.worker_id,
        max_concurrent_providers = limit,
        "worker consuming scan tasks"
    );

    let mut inflight: JoinSet<()> = JoinSet::new();
    // Which provider each in-flight task is scanning, keyed by task rather than carried back
    // as the task's return value — see [`release`].
    let mut slugs: HashMap<tokio::task::Id, String> = HashMap::new();
    let mut busy: HashSet<String> = HashSet::new();

    while !shutdown.is_cancelled() {
        while let Some(finished) = inflight.try_join_next_with_id() {
            release(finished, &mut slugs, &mut busy);
        }
        report_inflight(inflight.len());
        if inflight.len() >= limit {
            // Wait for a slot rather than spinning the poll loop against a full set.
            if let Some(finished) = inflight.join_next_with_id().await {
                release(finished, &mut slugs, &mut busy);
            }
            continue;
        }

        if let Some(msg) = queue.try_next(&busy).await {
            let task = match serde_json::from_slice::<ScanTaskMessage>(&msg.payload) {
                Ok(task) => task,
                Err(e) => {
                    tracing::warn!(error = %e, "undecodable task message; dropping");
                    if let Err(e) = msg.ack().await {
                        tracing::warn!(error = %e, "failed to ack message");
                    }
                    continue;
                }
            };
            let slug = task.provider_slug.clone();
            busy.insert(slug.clone());
            let id = inflight.spawn(run_task(Arc::clone(engine), msg, task)).id();
            slugs.insert(id, slug);
            continue;
        }

        // Nothing servable this round. The wait has to watch `inflight` as well as the clock:
        // every lane may be blocked by a provider whose task has *already finished*, and
        // joining it is the only thing that can free one. Waiting on the clock alone is how
        // this loop used to wedge permanently.
        let delay = queue.idle_delay();
        if let Some(finished) = wait_while_idle(&mut inflight, &shutdown, delay).await {
            release(finished, &mut slugs, &mut busy);
        }
    }

    if !inflight.is_empty() {
        tracing::info!(
            inflight = inflight.len(),
            "shutdown requested; draining in-flight scan tasks"
        );
    }
    while let Some(finished) = inflight.join_next_with_id().await {
        release(finished, &mut slugs, &mut busy);
    }
    report_inflight(0);
    tracing::info!(worker_id = %engine.worker_id, "worker stopping");
    Ok(())
}

/// Wait out a round that served nothing: until a task finishes, the backoff lapses, or
/// shutdown is requested. `Some` is a task to release.
///
/// Joining here is not an optimisation. A worker runs one task per provider, so a lane whose
/// provider is in flight is passed over — and the set of providers in flight is only ever
/// shrunk by this join. Sleeping on the clock alone left a finished task's provider marked
/// busy for as long as the wait, and when *every* lane with work was blocked that way, the
/// wait never ended: no lane could be served, so the loop never reached the join, so no lane
/// was ever unblocked.
///
/// Every future here is cancel-safe, which is the constraint that shapes it: `try_next` is
/// deliberately not among them, because cancelling a pull mid-fetch hands a message back and
/// burns one of its three deliveries (see [`queue`]).
async fn wait_while_idle(
    inflight: &mut JoinSet<()>,
    shutdown: &tokio_util::sync::CancellationToken,
    delay: Duration,
) -> Option<Result<(tokio::task::Id, ()), tokio::task::JoinError>> {
    tokio::select! {
        () = shutdown.cancelled() => None,
        // Guarded: `join_next_with_id` on an empty set resolves immediately with `None`,
        // which would turn this wait into a spin.
        finished = inflight.join_next_with_id(), if !inflight.is_empty() => finished,
        () = tokio::time::sleep(delay) => None,
    }
}

/// Report how many scan tasks are in flight.
///
/// Widened through `u32` rather than cast straight to `f64`: the direct cast is a pedantic
/// clippy failure, and the count is bounded by `max_concurrent_providers`, so the saturation
/// is unreachable rather than merely unlikely.
fn report_inflight(count: usize) {
    metrics::gauge!("scan_tasks_inflight").set(f64::from(u32::try_from(count).unwrap_or(u32::MAX)));
}

/// Free the provider slot a finished task held.
///
/// Keyed on [`tokio::task::Id`] rather than returned by the task itself, because a panicking
/// task returns nothing — `JoinError` still carries its id, so the slot is released on that
/// path too. Had the slug ridden back as the task's output, a panic would leave its provider
/// in `busy` forever, and a provider in `busy` is skipped by every future poll: one panic
/// would silently retire that provider for the life of the process, with only the panic
/// itself in the log and nothing connecting the two.
fn release(
    finished: Result<(tokio::task::Id, ()), tokio::task::JoinError>,
    slugs: &mut HashMap<tokio::task::Id, String>,
    busy: &mut HashSet<String>,
) {
    let id = match &finished {
        Ok((id, ())) => *id,
        Err(e) => e.id(),
    };
    if let Some(slug) = slugs.remove(&id) {
        busy.remove(&slug);
    }
    if let Err(e) = finished {
        tracing::error!(error = %e, "scan task panicked; released its provider slot");
    }
}

/// Run one claimed task to a terminal disposition, then settle its message.
///
/// Owns its message for the whole lifetime — heartbeat, retry decision and ack all live here,
/// so concurrent tasks cannot settle each other's messages. It also owns the two measurements
/// that explain the task afterwards: the stage reporter, and the fetch accounting scope, which
/// is entered here because a scope covers exactly the futures of the tokio task that opened it
/// and this is that task.
async fn run_task(engine: Arc<Engine>, msg: tankovault_bus::BrokerMessage, task: ScanTaskMessage) {
    // Wrapped here rather than inside the engine: ack lifetime belongs to whoever owns the
    // message, and doing it at this level means *every* task kind is covered — a 20k-entry
    // catalogue page today, whatever runs long tomorrow — instead of each slow path having to
    // remember.
    let started = std::time::Instant::now();
    let stage = engine::StageReporter::for_task(engine.pool.clone(), task.task_id);
    // Boxed: the composed future carries the whole dispatch state machine — a catalogue page's
    // entry vectors included — and this one is held across the consumer loop's `JoinSet`, where
    // an inline 35 KB future is 35 KB per concurrent provider on the stack.
    let (result, fetched) =
        tankovault_fetch::measured(Box::pin(tankovault_bus::with_ack_heartbeat(
            &msg,
            tankovault_bus::TASK_ACK_HEARTBEAT,
            handle_task(&engine, &task, &stage),
        )))
        .await;
    let elapsed = started.elapsed();
    metrics::histogram!(
        "scan_task_duration_seconds",
        "provider" => task.provider_slug.clone(),
        "scan" => task.mode.as_str(),
        "kind" => task.kind.as_str(),
    )
    .record(elapsed.as_secs_f64());

    let timings = stage.finish(fetched);
    record_stage_metrics(&task, &timings);
    let outcome = tankovault_db::repo::scans::TaskOutcome {
        duration_ms: i32::try_from(elapsed.as_millis()).unwrap_or(i32::MAX),
        timings: &timings,
    };

    match result {
        Ok(Handled::Declined) => {
            // Neither done nor failed: the run either finished without this task or was
            // cancelled, and both counters are already where they should be. Acked below so the
            // message stops being redelivered into a claim that will refuse it again.
            tracing::info!(
                provider = %task.provider_slug,
                task_id = %task.task_id,
                run_id = %task.run_id,
                "scan task declined; its run has settled or was cancelled"
            );
            settled(&task, "declined");
            if let Err(e) = msg.ack().await {
                tracing::warn!(error = %e, "failed to ack message");
            }
            return;
        }
        Ok(Handled::Executed) => {
            settled(&task, "completed");
            if let Err(e) = tankovault_db::repo::scans::complete_task(
                &engine.pool,
                task.task_id,
                Some(&outcome),
            )
            .await
            {
                tracing::warn!(
                    task_id = %task.task_id,
                    error = %e,
                    next = "the task stays unsettled until JetStream redelivers it",
                    "could not complete the task"
                );
            }
        }
        Err(e) => {
            if requeue_or_fail(&engine, &msg, &task, &e, &outcome).await == Disposition::Requeued {
                // Returns without acking or reporting progress: `retry_later` has already settled
                // the message, and the run stays open *for this task* — the idempotent writes make
                // the re-run a no-op for whatever it did do.
                return;
            }
        }
    }
    // Republish progress after the task settles (done or failed) so the control-plane
    // aggregator can finalise the run and the console can relay live progress over NATS
    // (design §12).
    engine.report_progress(task.run_id).await;
    if let Err(e) = msg.ack().await {
        tracing::warn!(error = %e, "failed to ack message");
    }
}

/// What became of a task that failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Disposition {
    /// Handed back for another delivery; the message is already settled.
    Requeued,
    /// Recorded as failed; the run continues without it.
    Failed,
}

/// Decide whether a failed task gets another delivery, and carry that decision out.
///
/// The retry budget is spent on failures the *provider* may recover from; anything else fails
/// immediately, because a worker that cannot reach Postgres has a problem no redelivery fixes.
async fn requeue_or_fail(
    engine: &Engine,
    msg: &tankovault_bus::BrokerMessage,
    task: &ScanTaskMessage,
    error: &anyhow::Error,
    outcome: &tankovault_db::repo::scans::TaskOutcome<'_>,
) -> Disposition {
    let deliveries = tankovault_bus::delivery_count(msg);
    if is_retryable(error) && deliveries < MAX_TASK_DELIVERIES {
        let delay = retry_delay(deliveries);
        log_task_failure(
            task,
            deliveries,
            error,
            &format!(
                "requeued; delivery {} of {MAX_TASK_DELIVERIES} follows in {}s",
                deliveries + 1,
                delay.as_secs()
            ),
        );
        if let Err(e) = tankovault_bus::retry_later(msg, delay).await {
            tracing::warn!(
                task_id = %task.task_id,
                error = %e,
                "could not requeue task; it will be redelivered when the ack deadline lapses"
            );
        }
        // The counter still moves, because "retrying" is the disposition an operator needs
        // separated from "threw it away".
        settled(task, "requeued");
        return Disposition::Requeued;
    }

    let next = if is_retryable(error) {
        format!(
            "gave up after {MAX_TASK_DELIVERIES} deliveries; recorded as failed and the run \
             continues without it"
        )
    } else {
        "will fail identically on replay; recorded as failed and the run continues without it"
            .to_owned()
    };
    log_task_failure(task, deliveries, error, &next);
    settled(task, "failed");
    let _ = tankovault_db::repo::scans::fail_task(
        &engine.pool,
        task.task_id,
        &error.to_string(),
        Some(outcome),
    )
    .await;
    Disposition::Failed
}

/// Publish the task's breakdown as metrics, so the same question the console answers per run is
/// answerable per provider over a week without reading `scan_tasks`.
fn record_stage_metrics(task: &ScanTaskMessage, timings: &tankovault_domain::StageTimings) {
    for (stage, millis) in &timings.stages {
        metrics::histogram!(
            "scan_stage_duration_seconds",
            "provider" => task.provider_slug.clone(),
            "kind" => task.kind.as_str(),
            "stage" => stage.clone(),
        )
        .record(seconds(*millis));
    }
    // The figure that answers "why is this slow" without any further digging: a provider whose
    // pace-wait dominates its fetch time is being crawled exactly as politely as configured.
    metrics::histogram!(
        "scan_task_pace_wait_seconds",
        "provider" => task.provider_slug.clone(),
    )
    .record(seconds(timings.pace_wait_ms));
}

/// Milliseconds as the seconds a histogram takes.
#[expect(
    clippy::cast_precision_loss,
    reason = "a millisecond count large enough to lose f64 precision is 285,000 years"
)]
fn seconds(millis: i64) -> f64 {
    millis as f64 / 1_000.0
}

/// Deliveries a scan task gets before its failure is treated as final.
///
/// Sized against what actually recovers between attempts — a challenge solve, a provider's
/// rate-limit window, a solver restart. Past that, "transient" is not a useful description of
/// the failure, and further attempts only delay the run's completion.
const MAX_TASK_DELIVERIES: u64 = 3;

/// Whether `err` is worth another delivery.
///
/// The adapter layer owns this judgement
/// ([`AdapterError::is_transient`](tankovault_adapters::AdapterError::is_transient)) because it
/// is the
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
/// Count a task reaching a terminal disposition.
///
/// Separate from `scan_tasks_served_total`, which a redelivery increments again: served
/// conflates throughput with retries, and the permanent-failure rate — the number worth
/// alerting on — is only derivable from `outcome="failed"` here.
fn settled(task: &ScanTaskMessage, outcome: &'static str) {
    metrics::counter!(
        "scan_tasks_settled_total",
        "provider" => task.provider_slug.clone(),
        "scan" => task.mode.as_str(),
        "outcome" => outcome,
    )
    .increment(1);
}

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

/// Whether the task's work actually ran, or the claim refused it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Handled {
    Executed,
    /// The claim was refused: the task had already settled, or its run has been cancelled. The
    /// message is settled without touching the run counters.
    Declined,
}

async fn handle_task(
    engine: &Engine,
    task: &ScanTaskMessage,
    stage: &engine::StageReporter,
) -> anyhow::Result<Handled> {
    let provider = tankovault_db::repo::providers::get(&engine.pool, task.provider_id).await?;
    // The claim is the cancellation check. `JetStream` holds this message independently of the
    // database, so an operator cancelling a run cannot unpublish its tasks — refusing to start
    // one here is the only place the cancellation takes effect.
    if !tankovault_db::repo::scans::claim_task(&engine.pool, task.task_id, &engine.worker_id)
        .await?
    {
        return Ok(Handled::Declined);
    }
    // A `CatalogPage` task fans out its children (and bumps the run total) before this
    // returns, so completing it — in `run_task`, once the accounting scope closes — cannot
    // finalise the run prematurely.
    engine.dispatch_task(&provider, task, stage).await?;
    Ok(Handled::Executed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tankovault_domain::{ProviderId, ScanMode, ScanRunId, ScanTaskId};

    /// Zero concurrency must not be honoured literally.
    ///
    /// It reads like "turn scanning off" but it deadlocks: the loop is never under the limit,
    /// so it never claims a task, so it never spawns one to get back under it. The worker sits
    /// idle against a full queue and nothing in the logs says why. Turning a provider off is
    /// what `providers.active` is for.
    #[test]
    fn a_concurrency_of_zero_is_clamped_rather_than_obeyed() {
        let cfg = WorkerConfig {
            max_concurrent_providers: 0,
            ..WorkerConfig::default()
        };
        assert_eq!(cfg.max_concurrent_providers(), 1);
        assert!(
            WorkerConfig::default().max_concurrent_providers() >= 1,
            "the shipped default must be able to run at least one task"
        );
    }

    /// A panicking task must release its provider, or that provider is never scanned again.
    ///
    /// The slug is tracked by `tokio::task::Id` precisely because a panicked task returns no
    /// value. Had it ridden back as the task's output, this path would leave the provider in
    /// `busy` forever — and `busy` is consulted before every pull, so the provider would be
    /// skipped silently for the life of the process, with only an unattributed panic in the
    /// log. This pins the recovery.
    #[tokio::test]
    async fn a_panicked_task_releases_its_provider_slot() {
        let mut inflight: JoinSet<()> = JoinSet::new();
        let mut slugs: HashMap<tokio::task::Id, String> = HashMap::new();
        let mut busy: HashSet<String> = HashSet::new();

        let id = inflight
            .spawn(async { panic!("adapter blew up mid-scan") })
            .id();
        slugs.insert(id, "kunmanga".to_owned());
        busy.insert("kunmanga".to_owned());

        let finished = inflight
            .join_next_with_id()
            .await
            .expect("the task was spawned, so it must be joinable");
        assert!(
            finished.is_err(),
            "the task panicked, so joining must error"
        );
        release(finished, &mut slugs, &mut busy);

        assert!(
            !busy.contains("kunmanga"),
            "a panicked task left its provider marked busy; it would never be polled again"
        );
        assert!(slugs.is_empty(), "the slug map must not leak entries");
    }

    /// The ordinary path frees the slot too.
    #[tokio::test]
    async fn a_completed_task_releases_its_provider_slot() {
        let mut inflight: JoinSet<()> = JoinSet::new();
        let mut slugs: HashMap<tokio::task::Id, String> = HashMap::new();
        let mut busy: HashSet<String> = HashSet::new();

        let id = inflight.spawn(async {}).id();
        slugs.insert(id, "demonicscans".to_owned());
        busy.insert("demonicscans".to_owned());

        let finished = inflight.join_next_with_id().await.expect("joinable");
        release(finished, &mut slugs, &mut busy);

        assert!(!busy.contains("demonicscans"));
        assert!(slugs.is_empty());
    }

    /// The idle wait must observe a task **finishing**, not only the clock.
    ///
    /// The bug this pins wedged the whole pool. `busy` is pruned only where the loop joins a
    /// finished task, and the poll it waited in blocked until some lane could be served — but a
    /// lane whose provider is in `busy` is passed over, so once every provider holding queued
    /// work was in `busy`, no lane could be served, the poll never returned, the loop never
    /// reached the join, and the providers whose tasks had *already finished* stayed blocked
    /// forever. It presents as a worker with nothing in flight sitting against a full queue,
    /// with no error anywhere: the pool simply stops consuming until it is restarted, and a
    /// three-provider deployment reaches it within one task each.
    #[tokio::test]
    async fn the_idle_wait_observes_a_task_finishing() {
        let shutdown = tokio_util::sync::CancellationToken::new();
        let mut inflight: JoinSet<()> = JoinSet::new();
        let mut slugs: HashMap<tokio::task::Id, String> = HashMap::new();
        let mut busy: HashSet<String> = HashSet::new();

        let id = inflight.spawn(async {}).id();
        slugs.insert(id, "kunmanga".to_owned());
        busy.insert("kunmanga".to_owned());

        // A backoff no test would sit through: reaching it means only the clock could have
        // ended the wait, which is the regression.
        let finished = tokio::time::timeout(
            Duration::from_secs(5),
            wait_while_idle(&mut inflight, &shutdown, Duration::from_secs(3600)),
        )
        .await
        .expect("the wait must end when a task finishes, not sleep out its backoff")
        .expect("a finished task must be handed back, or its provider is never released");

        release(finished, &mut slugs, &mut busy);
        assert!(
            busy.is_empty(),
            "the finished task's provider is still blocked; every lane it owns is unservable"
        );
    }

    /// With nothing in flight the wait is the backoff, and must not spin.
    ///
    /// `join_next_with_id` on an empty `JoinSet` resolves immediately, so an unguarded branch
    /// would return at once on every idle round — turning a five-second poll into a hot loop
    /// against the broker.
    #[tokio::test]
    async fn an_empty_pool_waits_out_the_backoff_instead_of_spinning() {
        let shutdown = tokio_util::sync::CancellationToken::new();
        let mut inflight: JoinSet<()> = JoinSet::new();

        let started = tokio::time::Instant::now();
        let finished = wait_while_idle(&mut inflight, &shutdown, Duration::from_millis(50)).await;

        assert!(finished.is_none(), "there was no task to hand back");
        assert!(
            started.elapsed() >= Duration::from_millis(50),
            "the wait returned early; with an empty pool it would poll the broker flat out"
        );
    }

    /// Shutdown ends the wait rather than being noticed a backoff later.
    #[tokio::test]
    async fn shutdown_ends_the_idle_wait() {
        let shutdown = tokio_util::sync::CancellationToken::new();
        shutdown.cancel();
        let mut inflight: JoinSet<()> = JoinSet::new();

        tokio::time::timeout(
            Duration::from_secs(5),
            wait_while_idle(&mut inflight, &shutdown, Duration::from_secs(3600)),
        )
        .await
        .expect("a cancelled token must end the wait immediately");
    }

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
