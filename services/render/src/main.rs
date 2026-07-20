//! # render service
//!
//! Optional headless-browser rendering for JS-rendered listing pages (design §9), and an
//! alternate [`ChallengeSolver`](tankovault_solver::ChallengeSolver) back-end for when
//! `FlareSolverr` is unavailable. It drives a long-lived `chromiumoxide` browser and
//! exposes:
//!
//! - `POST /v1/render { url, wait_selector?, wait_ms? }` → the rendered DOM + session.
//! - `POST /v1/solve  { url, provider, kind? }` → the `challenge-solver` contract, so the
//!   fetch pipeline can treat this service as a drop-in bypass back-end.
//!
//! The browser is launched lazily on first use, so `/health` and `/ready` come up even
//! when no Chrome binary is available; a render/solve then fails cleanly with `502`.

mod browser;
mod config;
mod solver;

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use tankovault_solver::{ChallengeSolver, SolveRequest};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;

use crate::browser::{BrowserManager, RenderOptions};
use crate::config::Config;
use crate::solver::ChromiumSolver;

#[derive(Clone)]
struct AppState {
    manager: Arc<BrowserManager>,
    solver: Arc<ChromiumSolver>,
    metrics: tankovault_observability::PrometheusHandle,
}

#[derive(Debug, Deserialize)]
struct RenderRequest {
    url: String,
    #[serde(default)]
    wait_selector: Option<String>,
    #[serde(default)]
    wait_ms: u64,
}

#[derive(Debug, Serialize)]
struct Cookie {
    name: String,
    value: String,
}

#[derive(Debug, Serialize)]
struct RenderResponse {
    url: String,
    final_url: String,
    html: String,
    cookies: Vec<Cookie>,
    user_agent: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cfg: Config = tankovault_config::load()?;
    let metrics = tankovault_observability::init(&cfg.telemetry)?;

    let manager = Arc::new(BrowserManager::new(cfg.render.clone()));
    let solver = Arc::new(ChromiumSolver::new(
        Arc::clone(&manager),
        cfg.render.session_ttl_secs,
        cfg.render.challenge_wait_ms,
    ));
    tracing::info!(
        backend = solver.backend_name(),
        headless = cfg.render.headless,
        "render service starting"
    );

    let state = AppState {
        manager,
        solver,
        metrics,
    };

    let app = Router::new()
        .route("/v1/render", post(render))
        .route("/v1/solve", post(solve))
        .route("/health", get(|| async { "ok" }))
        .route("/ready", get(|| async { "ok" }))
        .route("/metrics", get(metrics_handler))
        .with_state(state);

    let listener = TcpListener::bind(&cfg.bind_addr).await?;
    tracing::info!(addr = %cfg.bind_addr, "render service listening");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn render(
    State(state): State<AppState>,
    Json(req): Json<RenderRequest>,
) -> impl IntoResponse {
    let url = req.url.clone();
    let opts = RenderOptions {
        url: req.url,
        wait_selector: req.wait_selector,
        wait_ms: req.wait_ms,
    };
    match state.manager.render(opts).await {
        Ok(result) => {
            metrics::counter!("render_requests_total", "result" => "ok").increment(1);
            let resp = RenderResponse {
                url,
                final_url: result.final_url,
                html: result.html,
                cookies: result
                    .cookies
                    .into_iter()
                    .map(|(name, value)| Cookie { name, value })
                    .collect(),
                user_agent: result.user_agent,
            };
            (StatusCode::OK, Json(resp)).into_response()
        }
        Err(e) => {
            metrics::counter!("render_requests_total", "result" => "error").increment(1);
            tracing::warn!(%url, error = %e, "render failed");
            (StatusCode::BAD_GATEWAY, format!("render failed: {e}")).into_response()
        }
    }
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
