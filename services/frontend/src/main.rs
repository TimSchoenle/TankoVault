//! Owns the frontend service: it serves the compiled Dioxus WASM SPA and reverse-proxies `/v1/*`
//! (REST and SSE)
//! to the `api` service from one origin, via the shared [`HttpStack`] runtime.
//!
//! Two shared-stack concerns are deliberately *not* adopted here; see [`stack_security`] and the
//! rate-limiting note in [`main`].

mod csp;
mod proxy;
mod routes;
mod shell;
#[cfg(test)]
mod tests;

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use tankovault_config::{BrandingConfig, SecurityConfig};
use tankovault_frontend::config::{Config, FrontendConfig};
use tankovault_service::{CancellationToken, Health, HttpStack, MetricsRegistry};

use routes::build_router;

/// Shared state for the `/v1/*` proxy: the reusable client, the upstream origin, and the body
/// cap. Cheap to clone (the client is internally reference-counted).
#[derive(Clone)]
struct AppState {
    client: reqwest::Client,
    upstream: String,
    max_body_bytes: usize,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // First, before anything can build a rustls configuration: rustls cannot choose a provider
    // for itself in this graph and panics instead of erroring. See `tankovault_service::crypto`.
    tankovault_service::install_crypto_provider();

    // Before anything else: this may be Docker's HEALTHCHECK invoking the binary instead of
    // the server. `scratch` has no shell/wget, so the binary probing itself is the only probe.
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
    // The scrape lives on its own listener (`TANKOVAULT_METRICS__LISTEN`), so the public
    // port the browser reaches never serves it. Outside the reloadable runtime so a reload
    // does not rebind it.
    tankovault_service::spawn_metrics_server(metrics.clone(), shutdown.clone());

    tankovault_service::run_reloading(boot, &shutdown, |cfg, generation| {
        serve_once(cfg, metrics.clone(), generation)
    })
    .await
}

/// Builds and runs everything a configuration change rebuilds: the upstream client, the proxy
/// state, the router and the listener.
///
/// Returns when `shutdown` is cancelled, by the OS signal, or by the supervisor because the
/// configuration changed and this runtime is being replaced.
async fn serve_once(
    cfg: Arc<Config>,
    metrics: MetricsRegistry,
    shutdown: CancellationToken,
) -> anyhow::Result<()> {
    // Decompression off so the response body forwards byte-for-byte alongside its
    // `Content-Encoding` header, instead of being silently decoded and shipped mismatched.
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(cfg.frontend.connect_timeout_secs))
        .gzip(false)
        .brotli(false)
        .build()?;

    let state = AppState {
        client,
        upstream: cfg.frontend.api_upstream.trim_end_matches('/').to_owned(),
        max_body_bytes: cfg.frontend.max_body_bytes,
    };

    tracing::info!(
        static_dir = %cfg.frontend.static_dir,
        upstream = %state.upstream,
        bind = %cfg.bind_addr,
        "frontend serving the SPA and proxying /v1/* to the api"
    );

    // No rate limiter here: one page load fetches the shell and every hashed asset, so any
    // bucket tight enough to matter throttles a legit cold load. The API applies the limits
    // that protect state, and sees the real client via the X-Forwarded-For this hop appends.
    let health = upstream_health(&state);
    let app = build_app(&cfg.frontend, &cfg.branding, state, &metrics, health);
    tankovault_service::serve(&cfg.bind_addr, app, shutdown).await?;
    Ok(())
}

/// The shared [`HttpStack`]'s hardening config, derived, not operator-supplied.
///
/// `security_headers` must stay off: the shared middleware's `default-src 'none'` CSP is right
/// for a JSON API but blocks the WASM bundle from booting; this tier sends its own policy
/// ([`content_security_policy`]) on the app shell instead. `cors` is meaningless since the SPA
/// and API share an origin through this proxy. `max_body_bytes` is mapped from
/// `TANKOVAULT_FRONTEND__MAX_BODY_BYTES` so this cap and the proxy's buffering guard can't drift.
///
/// [`content_security_policy`]: csp::content_security_policy
fn stack_security(frontend: &FrontendConfig) -> SecurityConfig {
    SecurityConfig {
        security_headers: false,
        max_body_bytes: frontend.max_body_bytes,
        ..SecurityConfig::default()
    }
}

/// Readiness for this tier: is the `api` upstream reachable?
///
/// Previously nothing checked it, so a frontend whose upstream was gone still reported
/// healthy. `/health` (liveness) stays independent, since restarting this process can't fix
/// an unreachable API.
fn upstream_health(state: &AppState) -> Health {
    let client = state.client.clone();
    let url = format!("{}/health", state.upstream);
    Health::builder()
        .check_fn("api", move || {
            let client = client.clone();
            let url = url.clone();
            async move {
                match client.get(&url).send().await {
                    Ok(response) if response.status().is_success() => Ok(()),
                    Ok(response) => Err(format!("upstream answered {}", response.status())),
                    Err(error) => Err(error.to_string()),
                }
            }
        })
        .build()
}

/// Assemble the served application: the app router inside the shared middleware stack, plus
/// the ops probes merged *outside* it, so a body cap or a timeout can never make a healthy
/// replica look unhealthy to its orchestrator.
fn build_app(
    frontend: &FrontendConfig,
    branding: &BrandingConfig,
    state: AppState,
    metrics: &MetricsRegistry,
    health: Health,
) -> Router {
    HttpStack::new(&stack_security(frontend), metrics.clone())
        .apply(build_router(frontend, branding, state))
        .merge(tankovault_service::ops_router(health, metrics.clone()))
}
