//! # render service
//!
//! Optional headless-browser rendering for JS-rendered listing pages (design §9), and an
//! alternate [`tankovault_solver::ChallengeSolver`] back-end for when
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
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tankovault_service::problem::Problem;
use tankovault_service::{Health, HttpStack, MetricsRegistry, RateLimiter, RouteClassifier};
use tankovault_solver::ChallengeSolver;

use crate::browser::{BrowserManager, RenderOptions};
use crate::config::Config;
use crate::solver::ChromiumSolver;

#[derive(Clone)]
struct AppState {
    manager: Arc<BrowserManager>,
    solver: Arc<ChromiumSolver>,
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
    // Resolved before anything binds: a service in this tier that starts without a token
    // silently downgrades to the unauthenticated behaviour the token exists to remove, so
    // the production profile refuses to boot rather than serving privileged routes openly.
    let internal_token = tankovault_service::internal_auth::resolve(&cfg.internal)?;
    let shutdown = tankovault_service::install_shutdown();

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

    let state = AppState { manager, solver };
    let limiter = RateLimiter::from_config(&cfg.rate_limit, RouteClassifier::new(), None);

    // Serve the metrics scrape on its own port when configured, keeping it off the
    // request-facing listener.
    tankovault_service::spawn_metrics_server(metrics.clone(), shutdown.clone());

    let app = HttpStack::new(&cfg.security, metrics.clone())
        .with_rate_limit(limiter)
        .with_internal_auth(internal_token)
        .apply(
            Router::new()
                .route("/v1/render", post(render))
                .with_state(state.clone())
                // The solve contract itself is defined once, in `tankovault_solver::http`.
                .merge(tankovault_solver::http::solver_router(state.solver)),
        )
        // Readiness is "listening": the browser is launched lazily by design (see the
        // module docs), so probing it here would report a healthy replica as down until
        // its first render.
        .merge(tankovault_service::ops_router(
            Health::builder().build(),
            metrics,
        ));

    tankovault_service::serve(&cfg.bind_addr, app, shutdown).await?;
    Ok(())
}

/// Reject a target URL the renderer must not visit.
///
/// Chrome is handed the URL verbatim and returns the DOM *and the cookies it collected*, so
/// an unvalidated target is a full internal-network read: `file:///etc/passwd`,
/// `http://169.254.169.254/…` for cloud instance credentials, `http://api:8080/v1/admin/…`.
/// The same guard the crawler uses applies here — scheme allowlist plus the forbidden-range
/// table, including IP literals, which the DNS-level check cannot see.
///
/// This is a Rust-side check on the address the caller *named*. It does not survive a DNS
/// rebind, because Chrome resolves independently of this process; constraining that requires
/// `--host-resolver-rules` or an egress-restricted network namespace around the browser.
fn validate_target(raw: &str) -> Result<(), Box<Response>> {
    tankovault_domain::ssrf::validate_str(raw)
        .map(|_| ())
        .map_err(|e| {
            metrics::counter!("render_requests_total", "result" => "rejected").increment(1);
            tracing::warn!(url = %raw, error = %e, "refused a render target");
            // The reason is safe to return: it names only the caller's own URL and which policy
            // rule refused it, which is what makes a misconfigured provider debuggable.
            Box::new(
                Problem::new(
                    StatusCode::FORBIDDEN,
                    "refused_target",
                    format!("refused target: {e}"),
                )
                .into_response(),
            )
        })
}

async fn render(
    State(state): State<AppState>,
    Json(req): Json<RenderRequest>,
) -> impl IntoResponse {
    if let Err(rejection) = validate_target(&req.url) {
        return *rejection;
    }
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
            // The cause is in the log; the caller gets the shared RFC 9457 shape so the API's
            // `Upstream` client parses one error format from every internal peer (ARCH-12).
            Problem::bad_gateway().into_response()
        }
    }
}
