//! Owns scheduling and planning scans; workers execute them. Exposes `POST /internal/scans` for the
//! API's "Scan now" and runs a background scheduler that periodically triggers fast and full scans
//! of active providers.

mod aggregator;
mod cooldown;
mod dedupe;
mod leader;
mod policy;
mod reconcile;

mod handlers;
mod maintenance;
mod planning;
mod scheduler;

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::routing::{get, post};
use tankovault_bus::Bus;
use tankovault_control_plane::config::{Config, SchedulerConfig};
use tankovault_db::PgPool;
use tankovault_domain::ScanMode;
use tankovault_service::health::PostgresCheck;
use tankovault_service::problem::Problem;
use tankovault_service::{
    CancellationToken, FeatureGate, Health, HttpStack, InternalAuth, InternalRoute,
    MetricsRegistry, PostgresFlagSource, PostgresTunableSource, RateLimiter, RouteClassifier,
    RouteTable, TunableSet,
};

use handlers::{
    read_merge_policy, trigger_full_merge_sweep, trigger_merge_sweep, trigger_recsys_build,
    trigger_scan, write_merge_policy,
};
use scheduler::run_scheduler;

/// Trigger a scan run for one provider, or for every enabled provider.
const SCANS: &str = "/internal/scans";
/// Sweep the catalogue for duplicate series and merge what passes the threshold.
const MERGE_SWEEP: &str = "/internal/merge-sweep";
/// Starts an exhaustive sweep: rounds of [`MERGE_SWEEP`] until every shortlist is walked out.
const MERGE_SWEEP_ALL: &str = "/internal/merge-sweep-all";
/// Reads the automatic-merge policy the sweep applies.
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

// `Clone` so the scheduler loop can own a copy: the config lives behind an `Arc` shared
// with the reload supervisor, so it cannot be moved out of.

#[derive(Clone)]
struct AppState {
    pool: PgPool,
    bus: Bus,
    /// The operator's runtime switches, consulted per sweep instead of at boot so an
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
    /// How much work one full duplicate sweep may do, including how many series it may delete.
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
    // Bound, not discarded: the guard flushes queued Sentry events on the way out, and
    // dropping it here would close the client before the service serves anything.
    let _telemetry = tankovault_service::init_tracing(&boot.value.telemetry)?;
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

/// Builds and runs everything a configuration change rebuilds: the pool, the bus and Redis
/// connections, the scheduler and aggregator loops, the router and the listener.
///
/// Returns when `shutdown` is cancelled, by the OS signal, or by the supervisor because the
/// configuration changed and this runtime is being replaced. The background loops take the
/// same token, so they stop with the runtime that spawned them instead of accumulating one
/// scheduler per reload.
async fn serve_once(
    cfg: Arc<Config>,
    metrics: MetricsRegistry,
    shutdown: CancellationToken,
) -> anyhow::Result<()> {
    // Resolved before anything binds: starting without identity configured would silently serve
    // privileged routes unauthenticated, so the production profile refuses to boot instead.
    let internal_auth = tankovault_service::internal_auth::resolve(&cfg.internal)?;

    let db = &cfg.database;
    let pool = tankovault_db::connect(
        &db.url,
        tankovault_db::PoolSettings::new(db.max_connections, db.acquire_timeout_secs),
    )
    .await?;
    tankovault_service::metrics::spawn_pool_sampler(pool.clone(), shutdown.clone());

    cfg.nats.validate()?;
    let bus = Bus::connect(&cfg.nats.url, internal_auth.tls.as_ref()).await?;
    bus.ensure_streams(cfg.nats.events_max_age()).await?;

    // Loaded before the scheduler starts so the first post-restart sweep respects stored
    // flags instead of briefly running against defaults.
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

// The scheduler's derived budgets, as free functions instead of an inherent `impl`: the config
// type lives in this crate's library (`config-contract` has to reach it), and the types these
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
/// this runs than the full sweep does. That ceiling is really a *rate*: the bound on how
/// many rows a bad threshold can delete before an operator looks, so running the rotation
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
    /// rotation pass would silently multiply that bound by the number of rotations in an hour:
    /// the ceiling would still read `200` in the configuration while permitting five times as
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
    /// ceiling instead of a share of a cadence that never runs.
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
    /// discovery one rounds its share to zero, which would not be a conservative ceiling but a
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
