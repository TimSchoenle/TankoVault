//! # challenge-solver service
//!
//! The modular bot-management bypass tier (design §9). Exposes a small HTTP contract
//! (`POST /v1/solve`) consumed by the worker's `SolvingFetcher`, fronting a pluggable
//! [`ChallengeSolver`] back-end (`FlareSolverr` by default). Isolating the browser/solver
//! runtime here lets it scale independently of the workers.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;
use std::sync::Arc;
use tankovault_config::TelemetryConfig;
use tankovault_service::{Health, HttpStack, MetricsRegistry, RateLimiter, RouteClassifier};
use tankovault_solver::{ChallengeSolver, FlareSolverrSolver, SolveRequest};

#[derive(Debug, Deserialize)]
struct Config {
    #[serde(default = "default_bind")]
    bind_addr: String,
    telemetry: TelemetryConfig,
    solver: SolverBackendConfig,
    /// Edge hardening for this internal service: body cap, request timeout, security
    /// headers. CORS stays off — nothing browser-originated calls it.
    #[serde(default)]
    security: tankovault_config::SecurityConfig,
    /// Inbound rate limiting. On by default even here: a runaway worker retry loop is as
    /// capable of exhausting the solver pool as a hostile client.
    #[serde(default)]
    rate_limit: tankovault_config::RateLimitConfig,
    /// Prometheus metrics. Togglable; disabling installs no recorder.
    #[serde(default)]
    metrics: tankovault_config::MetricsConfig,
    /// Shared secret every caller must present. `/v1/solve` fetches a caller-supplied URL
    /// and returns the body, which is an SSRF primitive for anyone who can reach the port.
    #[serde(default)]
    internal: tankovault_config::InternalAuthConfig,
}

fn default_bind() -> String {
    "0.0.0.0:8090".to_owned()
}

#[derive(Debug, Deserialize)]
struct SolverBackendConfig {
    /// Back-end selector. Only `flaresolverr` is wired today; the trait makes adding
    /// another (headless render, commercial) a drop-in.
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
    let cfg: Config = tankovault_config::load()?;
    tankovault_service::init_tracing(&cfg.telemetry)?;
    let metrics = MetricsRegistry::install(&cfg.metrics)?;
    // Resolved before anything binds: a service in this tier that starts without a token
    // silently downgrades to the unauthenticated behaviour the token exists to remove, so
    // the production profile refuses to boot rather than serving privileged routes openly.
    let internal_token = tankovault_service::internal_auth::resolve(&cfg.internal)?;
    let shutdown = tankovault_service::install_shutdown();

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

    // Serve the metrics scrape on its own port when configured, keeping it off the
    // request-facing listener.
    tankovault_service::spawn_metrics_server(metrics.clone(), shutdown.clone());

    let app = HttpStack::new(&cfg.security, metrics.clone())
        .with_rate_limit(limiter)
        .with_internal_auth(internal_token)
        .apply(
            Router::new()
                .route("/v1/solve", post(solve))
                .with_state(state),
        )
        // This service holds no state of its own and reaches no database, so readiness is
        // simply "listening" — the upstream FlareSolverr is deliberately not probed: it is
        // launched lazily and a solve already degrades to `502` when it is unavailable.
        .merge(tankovault_service::ops_router(
            Health::builder().build(),
            metrics,
        ));

    tankovault_service::serve(&cfg.bind_addr, app, shutdown).await?;
    Ok(())
}

/// Reject a target URL the solver must not visit.
///
/// `SolveRequest.url` used to reach FlareSolverr's `request.get` unvalidated, so anything
/// that could open a socket to this service had an arbitrary-URL fetcher that returns the
/// body — cloud instance metadata, internal admin endpoints, `file:` reads. Same policy the
/// crawler applies, including IP literals.
///
/// The check binds the address the caller *named*. FlareSolverr resolves independently, so
/// this does not survive a DNS rebind; that needs egress restriction around the solver
/// container.
fn validate_target(raw: &str) -> Result<(), Response> {
    tankovault_domain::ssrf::validate_str(raw)
        .map(|_| ())
        .map_err(|e| {
            metrics::counter!("solve_attempts_total", "result" => "rejected").increment(1);
            tracing::warn!(url = %raw, error = %e, "refused a solve target");
            (StatusCode::FORBIDDEN, format!("refused target: {e}")).into_response()
        })
}

async fn solve(State(state): State<AppState>, Json(req): Json<SolveRequest>) -> impl IntoResponse {
    if let Err(rejection) = validate_target(&req.url) {
        return rejection;
    }
    let provider = req.provider.clone();
    match state.solver.solve(req).await {
        Ok(outcome) => {
            metrics::counter!("solve_attempts_total", "result" => "ok").increment(1);
            (StatusCode::OK, Json(outcome)).into_response()
        }
        Err(e) => {
            metrics::counter!("solve_attempts_total", "result" => "error").increment(1);
            tracing::warn!(%provider, error = %e, "solve failed");
            (StatusCode::BAD_GATEWAY, format!("solve failed: {e}")).into_response()
        }
    }
}
