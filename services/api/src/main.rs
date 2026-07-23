//! # api service
//!
//! The public edge (design §11): Axum REST + JSON, JWT auth with rotating refresh
//! cookies, RBAC-gated admin routes, and link resolution at read time. Tower middleware
//! provides tracing, CORS, and compression. This binary is a thin entrypoint — the route
//! table and app state live in the `tankovault_api` library (`src/lib.rs`), which also
//! exposes the `openapi` schema export `xtask openapi` uses to regenerate the frontend's
//! generated wire types.

use std::sync::Arc;
use tankovault_api::AppState;
use tokio::net::TcpListener;

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
    let bus = tankovault_api::connect_bus(cfg.nats.as_ref()).await;

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

    let app = tankovault_api::build_router(state);

    let listener = TcpListener::bind(&cfg.bind_addr).await?;
    tracing::info!(addr = %cfg.bind_addr, "api listening");
    axum::serve(listener, app).await?;
    Ok(())
}
