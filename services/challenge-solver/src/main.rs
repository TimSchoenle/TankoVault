//! Bot-management bypass service: exposes `POST /v1/solve` over a pluggable
//! [`ChallengeSolver`] back-end (`FlareSolverr` by default).

use serde::Deserialize;
use std::sync::Arc;
use tankovault_config::TelemetryConfig;
use tankovault_service::{
    CancellationToken, Health, HttpStack, MetricsRegistry, RateLimiter, RouteClassifier,
};
use tankovault_solver::{ChallengeSolver, FlareSolverrSolver};

#[derive(Debug, Deserialize)]
struct Config {
    #[serde(default = "default_bind")]
    bind_addr: String,
    telemetry: TelemetryConfig,
    solver: SolverBackendConfig,
    /// Edge hardening: body cap, timeout, security headers. CORS stays off — nothing
    /// browser-originated calls this service.
    #[serde(default)]
    security: tankovault_config::SecurityConfig,
    /// Inbound rate limiting. On by default: a runaway worker retry loop can exhaust the
    /// solver pool as easily as a hostile client.
    #[serde(default)]
    rate_limit: tankovault_config::RateLimitConfig,
    /// Prometheus metrics; disabling installs no recorder.
    #[serde(default)]
    metrics: tankovault_config::MetricsConfig,
    /// Shared secret every caller must present. `/v1/solve` fetches a caller-supplied URL
    /// and returns the body — an SSRF primitive for anyone who can reach the port.
    #[serde(default)]
    internal: tankovault_config::InternalAuthConfig,
}

fn default_bind() -> String {
    "0.0.0.0:8090".to_owned()
}

#[derive(Debug, Deserialize)]
struct SolverBackendConfig {
    /// Back-end selector; only `flaresolverr` is wired today.
    #[serde(default = "default_backend")]
    backend: String,
    /// `FlareSolverr` base endpoint, e.g. `http://flaresolverr:8191`.
    flaresolverr_endpoint: String,
    #[serde(default = "default_timeout")]
    max_timeout_ms: u64,
    #[serde(default = "default_ttl")]
    session_ttl_secs: u64,
}

fn default_backend() -> String {
    "flaresolverr".to_owned()
}
fn default_timeout() -> u64 {
    60_000
}
fn default_ttl() -> u64 {
    900
}

#[derive(Clone)]
struct AppState {
    solver: Arc<dyn ChallengeSolver>,
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
    let internal_token = tankovault_service::internal_auth::resolve(&cfg.internal)?;

    let solver: Arc<dyn ChallengeSolver> = match cfg.solver.backend.as_str() {
        "flaresolverr" => Arc::new(FlareSolverrSolver::new(
            cfg.solver.flaresolverr_endpoint.clone(),
            cfg.solver.max_timeout_ms,
            cfg.solver.session_ttl_secs,
        )),
        other => anyhow::bail!("unsupported solver backend: {other}"),
    };
    tracing::info!(backend = solver.backend_name(), "challenge-solver starting");

    let state = AppState { solver };
    let limiter = RateLimiter::from_config(&cfg.rate_limit, RouteClassifier::new(), None);

    let app = HttpStack::new(&cfg.security, metrics.clone())
        .with_rate_limit(limiter)
        .with_internal_auth(internal_token)
        // Contract shared with `render` — see `tankovault_solver::http`.
        .apply(tankovault_solver::http::solver_router(state.solver))
        // Readiness is just "listening": FlareSolverr is launched lazily and deliberately
        // not probed, since a solve already degrades to `502` when it's unavailable.
        .merge(tankovault_service::ops_router(
            Health::builder().build(),
            metrics,
        ));

    tankovault_service::serve(&cfg.bind_addr, app, shutdown).await?;
    Ok(())
}
