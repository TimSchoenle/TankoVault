//! Bot-management bypass service: exposes `POST /v1/solve` over a pluggable
//! [`ChallengeSolver`] back-end (TRAWL by default).

use std::sync::Arc;
use tankovault_challenge_solver::config::Config;
use tankovault_service::{
    CancellationToken, Health, HttpStack, InternalRoute, MetricsRegistry, RateLimiter,
    RouteClassifier,
};
use tankovault_solver::{ChallengeSolver, TrawlSolver};

/// Who may reach this service's routes.
///
/// `worker` alone: solving fetches a caller-supplied URL, so this is an arbitrary-URL fetch
/// primitive for anything that can reach the port, and the crawl path is the only legitimate
/// user. The entry itself comes from `tankovault_solver::http` so the route this service mounts
/// and the route it authorises cannot be spelled differently.
static INTERNAL_ROUTES: &[InternalRoute] = &[tankovault_solver::http::solve_route(&["worker"])];

#[derive(Clone)]
struct AppState {
    solver: Arc<dyn ChallengeSolver>,
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
    // Own port keeps the scrape off the request-facing listener. Outside the reloadable
    // runtime so a reload does not rebind it.
    tankovault_service::spawn_metrics_server(metrics.clone(), shutdown.clone());

    tankovault_service::run_reloading(boot, &shutdown, |cfg, generation| {
        serve_once(cfg, metrics.clone(), generation)
    })
    .await
}

/// Build and run everything a configuration change rebuilds: the solver back-end, the router
/// and the listener.
///
/// Returns when `shutdown` is cancelled — by the OS signal, or by the supervisor because the
/// configuration changed and this runtime is being replaced.
async fn serve_once(
    cfg: Arc<Config>,
    metrics: MetricsRegistry,
    shutdown: CancellationToken,
) -> anyhow::Result<()> {
    // Resolved before anything binds: starting without a token would silently serve
    // privileged routes unauthenticated, so the production profile refuses to boot instead.
    let internal_auth = tankovault_service::internal_auth::resolve(&cfg.internal)?;

    let solver: Arc<dyn ChallengeSolver> = match cfg.solver.backend.as_str() {
        "trawl" => Arc::new(TrawlSolver::new(
            cfg.solver.trawl_endpoint.clone(),
            cfg.solver.max_timeout_ms,
            cfg.solver.session_ttl_secs,
        )),
        other => anyhow::bail!("unsupported solver backend: {other}"),
    };
    tracing::info!(backend = solver.backend_name(), "challenge-solver starting");

    let state = AppState { solver };
    let limiter = RateLimiter::from_config(&cfg.rate_limit, RouteClassifier::new(), None);
    let health = Health::builder().build();

    let app = HttpStack::new(&cfg.security, metrics.clone())
        .with_rate_limit(limiter)
        .with_internal_auth(Some(tankovault_service::InternalAuth::new(
            &internal_auth,
            tankovault_service::RouteTable(INTERNAL_ROUTES),
        )))
        // Contract shared with `render` — see `tankovault_solver::http`.
        .apply(tankovault_solver::http::solver_router(state.solver))
        // Readiness is just "listening": TRAWL has its own `/health` gate on the browser pool
        // and is deliberately not probed from here, since a solve already degrades to `502`
        // when it's unavailable.
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
