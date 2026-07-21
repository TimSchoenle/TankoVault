//! # api service
//!
//! The public edge (design §11): Axum REST + JSON, JWT auth with rotating refresh
//! cookies, RBAC-gated admin routes, and link resolution at read time. Tower middleware
//! provides tracing, CORS, and compression.

// Handlers are `pub` so the router can name them, but a binary crate has no external
// surface — the `unreachable_pub` lint is pure noise here.
#![allow(unreachable_pub)]

mod admin;
mod auth;
mod error;
mod me;
mod series;
mod state;

use axum::Router;
use axum::routing::{get, patch, post, put};
use state::AppState;
use std::sync::Arc;
use tokio::net::TcpListener;
use tower_http::compression::CompressionLayer;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

#[derive(Debug, serde::Deserialize)]
struct Config {
    database: tankovault_config::DatabaseConfig,
    telemetry: tankovault_config::TelemetryConfig,
    auth: AuthConfig,
    #[serde(default = "default_bind")]
    bind_addr: String,
    #[serde(default = "default_control_plane")]
    control_plane_url: String,
    #[serde(default = "default_sync")]
    sync_url: String,
    #[serde(default = "default_challenge_solver")]
    challenge_solver_url: String,
    /// NATS connection for live SSE relay. Optional: when absent or unreachable the API
    /// still serves every other route; only `/v1/me/stream` degrades.
    #[serde(default)]
    nats: Option<tankovault_config::NatsConfig>,
}

#[derive(Debug, serde::Deserialize)]
struct AuthConfig {
    jwt_secret: String,
    #[serde(default = "default_access_minutes")]
    access_ttl_minutes: i64,
    #[serde(default = "default_refresh_days")]
    refresh_ttl_days: i64,
    #[serde(default)]
    cookie_secure: bool,
}

fn default_bind() -> String {
    "0.0.0.0:8080".to_owned()
}
fn default_control_plane() -> String {
    "http://control-plane:8081".to_owned()
}
fn default_sync() -> String {
    "http://sync:8083".to_owned()
}
fn default_challenge_solver() -> String {
    "http://challenge-solver:8090".to_owned()
}
fn default_access_minutes() -> i64 {
    15
}
fn default_refresh_days() -> i64 {
    30
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cfg: Config = tankovault_config::load()?;
    let metrics = tankovault_observability::init(&cfg.telemetry)?;

    let pool = tankovault_db::connect(
        &cfg.database.url,
        cfg.database.max_connections,
        cfg.database.acquire_timeout_secs,
    )
    .await?;

    // Connect to NATS for the live SSE relay. A broker outage must not stop the public edge
    // from booting, so a failure here degrades the feature to `503` rather than aborting.
    let bus = connect_bus(cfg.nats.as_ref()).await;

    let state = AppState {
        pool,
        jwt_secret: Arc::new(cfg.auth.jwt_secret.into_bytes()),
        access_ttl: time::Duration::minutes(cfg.auth.access_ttl_minutes),
        refresh_ttl: time::Duration::days(cfg.auth.refresh_ttl_days),
        control_plane_url: cfg.control_plane_url,
        sync_url: cfg.sync_url,
        challenge_solver_url: cfg.challenge_solver_url,
        bus,
        http: reqwest::Client::new(),
        metrics,
        cookie_secure: cfg.auth.cookie_secure,
    };

    let app = build_router(state);

    let listener = TcpListener::bind(&cfg.bind_addr).await?;
    tracing::info!(addr = %cfg.bind_addr, "api listening");
    axum::serve(listener, app).await?;
    Ok(())
}

/// Assemble the full route table and middleware stack. Kept out of `main` so the router
/// wiring stays readable as endpoints grow (frontend §9 added the reading-dashboard,
/// account, and console-users routes here).
fn build_router(state: AppState) -> Router {
    Router::new()
        // auth
        .route("/v1/auth/register", post(auth::register))
        .route("/v1/auth/login", post(auth::login))
        .route("/v1/auth/refresh", post(auth::refresh))
        .route("/v1/auth/logout", post(auth::logout))
        // public series
        .route("/v1/series", get(series::list))
        .route("/v1/series/{id}", get(series::detail))
        .route("/v1/series/{id}/chapters", get(series::chapters))
        .route("/v1/tags", get(series::tags))
        // public provider list for the Discover filter (§9.3)
        .route("/v1/providers", get(series::providers))
        // me
        .route("/v1/me/watchlist", get(me::watchlist))
        .route(
            "/v1/me/watchlist/{series_id}",
            put(me::put_watchlist).delete(me::delete_watchlist),
        )
        .route("/v1/me/progress/{series_id}", put(me::put_progress))
        .route("/v1/me/feed", get(me::feed))
        // reading dashboard + recommendations + stats (§9.3)
        .route("/v1/me/continue", get(me::continue_reading))
        .route("/v1/me/recommendations", get(me::recommendations))
        .route("/v1/me/stats", get(me::stats))
        // account settings (§9.4)
        .route("/v1/me/profile", patch(me::patch_profile))
        .route("/v1/me/sessions", get(me::sessions))
        .route("/v1/me/sessions/{id}", axum::routing::delete(me::delete_session))
        .route(
            "/v1/me/notification-prefs",
            get(me::notification_prefs).put(me::put_notification_prefs),
        )
        .route("/v1/me/notifications", get(me::notifications))
        .route("/v1/me/notifications/read", post(me::mark_read))
        // live per-user notification stream (SSE; token in query — EventSource cannot set headers)
        .route("/v1/me/stream", get(me::stream))
        // me — AniList external sync (proxied to the sync service)
        .route("/v1/me/sync/anilist/authorize", get(me::sync_authorize_url))
        .route("/v1/me/sync/anilist/callback", get(me::sync_callback))
        .route("/v1/me/sync/anilist/push", post(me::sync_push))
        .route("/v1/me/sync/anilist/pull", post(me::sync_pull))
        // admin
        .route("/v1/admin/stats", get(admin::system_stats))
        .route("/v1/admin/audit", get(admin::audit_log))
        .route(
            "/v1/admin/providers",
            get(admin::list_providers).post(admin::create_provider),
        )
        .route("/v1/admin/providers/stats", get(admin::provider_stats))
        .route(
            "/v1/admin/providers/{id}",
            patch(admin::update_provider).delete(admin::delete_provider),
        )
        .route(
            "/v1/admin/providers/{id}/state",
            post(admin::set_provider_state),
        )
        .route("/v1/admin/providers/{id}/test", post(admin::test_adapter))
        .route(
            "/v1/admin/providers/{id}/resolve",
            post(admin::resolve_provider),
        )
        .route("/v1/admin/users", get(admin::list_users))
        .route(
            "/v1/admin/scans",
            get(admin::list_scans).post(admin::trigger_scan),
        )
        .route("/v1/admin/scan-failures", get(admin::scan_failures))
        .route("/v1/admin/scans/stream", get(admin::scan_stream))
        .route("/v1/admin/scans/{run_id}", get(admin::get_scan))
        .route(
            "/v1/admin/merge-candidates",
            get(admin::list_merge_candidates),
        )
        .route(
            "/v1/admin/merge-candidates/dismiss",
            post(admin::dismiss_merge_candidate),
        )
        .route("/v1/admin/series/merge", post(admin::merge_series))
        // ops
        .route("/health", get(|| async { "ok" }))
        .route("/ready", get(|| async { "ok" }))
        .route("/metrics", get(metrics_handler))
        .layer(CompressionLayer::new())
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn metrics_handler(axum::extract::State(state): axum::extract::State<AppState>) -> String {
    state.metrics.render()
}

/// Best-effort connection to NATS for the live notification relay. Returns `None` (with a
/// log line) when NATS is unconfigured or unreachable, so `/v1/me/stream` degrades to `503`
/// while the rest of the edge keeps serving.
async fn connect_bus(nats: Option<&tankovault_config::NatsConfig>) -> Option<tankovault_bus::Bus> {
    let Some(nats) = nats else {
        tracing::info!("no NATS configured; /v1/me/stream disabled");
        return None;
    };
    match tankovault_bus::Bus::connect(&nats.url).await {
        Ok(bus) => {
            tracing::info!("connected to NATS for live notification relay");
            Some(bus)
        }
        Err(e) => {
            tracing::warn!(error = %e, "NATS unreachable; /v1/me/stream disabled");
            None
        }
    }
}
