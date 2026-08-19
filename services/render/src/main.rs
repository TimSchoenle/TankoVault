//! Render service: headless-browser rendering for JS-heavy listing pages, and an
//! alternate [`tankovault_solver::ChallengeSolver`] back-end for when TRAWL is
//! unavailable.

mod browser;
mod solver;

// Re-exported into the binary's own root so `crate::config::…` keeps resolving in the modules
// beside this one: the type itself lives in the library, where `config-contract` can reach it.
use tankovault_render::config;

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tankovault_service::problem::Problem;
use tankovault_service::{
    CancellationToken, Health, HttpStack, InternalAuth, InternalRoute, MetricsRegistry,
    RateLimiter, RouteClassifier, RouteTable,
};
use tankovault_solver::ChallengeSolver;

/// Render a caller-supplied URL in a headless browser and return the resulting DOM.
const RENDER_PATH: &str = "/v1/render";

/// Who may reach this service's routes.
///
/// `worker` alone, and this is the service where that matters most: both routes fetch a
/// caller-supplied URL, so reaching either is an arbitrary-URL fetch primitive on the internal
/// network (SEC audit — `POST /v1/render {"url":"file:///etc/passwd"}`). Under one shared token
/// every service in the tier could drive it; now only the one that crawls can.
static INTERNAL_ROUTES: &[InternalRoute] = &[
    InternalRoute {
        method: axum::http::Method::POST,
        path: RENDER_PATH,
        callers: &["worker"],
    },
    tankovault_solver::http::solve_route(&["worker"]),
];

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

/// Build and run everything a configuration change rebuilds: the browser manager, the solver,
/// the router and the listener.
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
    let health = Health::builder().build();

    let app = HttpStack::new(&cfg.security, metrics.clone())
        .with_rate_limit(limiter)
        .with_internal_auth(Some(InternalAuth::new(
            &internal_auth,
            RouteTable(INTERNAL_ROUTES),
        )))
        .apply(
            Router::new()
                .route(RENDER_PATH, post(render))
                .with_state(state.clone())
                // The solve contract itself is defined once, in `tankovault_solver::http`.
                .merge(tankovault_solver::http::solver_router(state.solver)),
        )
        // Readiness is "listening": the browser launches lazily, so probing it here would
        // report a healthy replica as down until its first render.
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

/// Rejects a target URL the renderer must not visit.
///
/// Chrome fetches the URL verbatim and returns the DOM and cookies, so an unvalidated
/// target is a full internal-network read (`file:///`, cloud metadata, internal APIs).
/// Uses the crawler's scheme allowlist plus the forbidden-range check, including IP
/// literals. This is a Rust-side check on the address as *named*; it does not survive a
/// DNS rebind, since Chrome resolves independently of this process.
fn validate_target(raw: &str) -> Result<(), Box<Response>> {
    tankovault_domain::ssrf::validate_str(raw)
        .map(|_| ())
        .map_err(|e| {
            metrics::counter!("render_requests_total", "result" => "rejected").increment(1);
            tracing::warn!(url = %raw, error = %e, "refused a render target");
            // Safe to return: it names only the caller's URL and which rule refused it.
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
            // Cause is in the log; caller gets the shared RFC 9457 shape every internal
            // peer uses.
            Problem::bad_gateway().into_response()
        }
    }
}
