//! # challenge-solver service
//!
//! The modular bot-management bypass tier (design §9). Exposes a small HTTP contract
//! (`POST /v1/solve`) consumed by the worker's `SolvingFetcher`, fronting a pluggable
//! [`ChallengeSolver`] back-end (`FlareSolverr` by default). Isolating the browser/solver
//! runtime here lets it scale independently of the workers.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use std::sync::Arc;
use tankovault_config::TelemetryConfig;
use tankovault_solver::{ChallengeSolver, FlareSolverrSolver, SolveRequest};
use tokio::net::TcpListener;

#[derive(Debug, Deserialize)]
struct Config {
    #[serde(default = "default_bind")]
    bind_addr: String,
    telemetry: TelemetryConfig,
    solver: SolverBackendConfig,
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
    metrics: tankovault_observability::PrometheusHandle,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cfg: Config = tankovault_config::load()?;
    let metrics = tankovault_observability::init(&cfg.telemetry)?;

    let solver: Arc<dyn ChallengeSolver> = match cfg.solver.backend.as_str() {
        "flaresolverr" => Arc::new(FlareSolverrSolver::new(
            cfg.solver.flaresolverr_endpoint.clone(),
            cfg.solver.max_timeout_ms,
            cfg.solver.session_ttl_secs,
        )),
        other => anyhow::bail!("unsupported solver backend: {other}"),
    };
    tracing::info!(backend = solver.backend_name(), "challenge-solver starting");

    let state = AppState { solver, metrics };

    let app = Router::new()
        .route("/v1/solve", post(solve))
        .route("/health", get(|| async { "ok" }))
        .route("/ready", get(|| async { "ok" }))
        .route("/metrics", get(metrics_handler))
        .with_state(state);

    let listener = TcpListener::bind(&cfg.bind_addr).await?;
    tracing::info!(addr = %cfg.bind_addr, "challenge-solver listening");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn solve(State(state): State<AppState>, Json(req): Json<SolveRequest>) -> impl IntoResponse {
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

async fn metrics_handler(State(state): State<AppState>) -> impl IntoResponse {
    state.metrics.render()
}
