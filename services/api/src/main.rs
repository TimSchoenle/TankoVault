//! # api service
//!
//! The public edge (design §11): Axum REST + JSON, JWT auth with rotating refresh
//! cookies, RBAC-gated admin routes, and link resolution at read time. This binary is a
//! thin entrypoint — the route table and app state live in the `tankovault_api` library
//! (`src/lib.rs`), which also exposes the `openapi` schema export `xtask openapi` uses to
//! regenerate the frontend's generated wire types.
//!
//! Everything cross-cutting (rate limiting, CORS, security headers, request ids, metrics,
//! timeouts, body caps, health probes, graceful shutdown) comes from `tankovault-service`.

use std::sync::Arc;
use std::time::Duration;
use tankovault_api::AppState;
use tankovault_service::{Health, MetricsRegistry, PostgresAuditSink, health::PostgresCheck};

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
    /// Redis, used for cross-replica rate-limit counters. Optional: without it the
    /// limiter falls back to per-replica in-memory counters.
    #[serde(default)]
    redis: Option<tankovault_config::RedisConfig>,
    /// Transactional email (welcome on registration, password reset). Optional: when
    /// unconfigured a no-op mailer is used and those flows silently skip sending.
    #[serde(default)]
    email: tankovault_config::EmailConfig,
    /// Edge hardening: CORS allowlist, body cap, request timeout, security headers.
    #[serde(default)]
    security: tankovault_config::SecurityConfig,
    /// Inbound rate limiting. Togglable; see `tankovault_config::RateLimitConfig`.
    #[serde(default)]
    rate_limit: tankovault_config::RateLimitConfig,
    /// Prometheus metrics. Togglable; disabling installs no recorder at all.
    #[serde(default)]
    metrics: tankovault_config::MetricsConfig,
    /// Audit trail. Togglable; disabling installs a no-op sink.
    #[serde(default)]
    audit: tankovault_config::AuditConfig,
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

/// Rows deleted per audit-retention sweep.
///
/// Bounded so a first sweep over a long-neglected table cannot hold locks long enough to
/// stall the writers appending to it; the sweep simply catches up over successive runs.
const AUDIT_PRUNE_BATCH: i64 = 10_000;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cfg: Config = tankovault_config::load()?;
    tankovault_service::init_tracing(&cfg.telemetry)?;
    let metrics = MetricsRegistry::install(&cfg.metrics)?;
    let shutdown = tankovault_service::install_shutdown();

    let pool = tankovault_db::connect(
        &cfg.database.url,
        cfg.database.max_connections,
        cfg.database.acquire_timeout_secs,
    )
    .await?;

    // Connect to NATS for the live SSE relay. A broker outage must not stop the public edge
    // from booting, so a failure here degrades the feature to `503` rather than aborting.
    let bus = tankovault_api::connect_bus(cfg.nats.as_ref()).await;

    // Likewise Redis: it only sharpens rate limiting across replicas, so an outage
    // downgrades to per-replica counters instead of refusing to start.
    let redis = connect_redis(cfg.redis.as_ref()).await;

    // Build the transactional email back-end. A missing/invalid relay degrades to a no-op
    // mailer (logs and drops) so the edge still boots and login/registration keep working.
    let mailer = tankovault_email::build(&cfg.email);

    let audit = build_audit_sink(&pool, &cfg.audit);
    spawn_audit_retention(&pool, &cfg.audit, shutdown.clone());

    let state = AppState {
        pool: pool.clone(),
        jwt_secret: Arc::new(cfg.auth.jwt_secret.into_bytes()),
        access_ttl: time::Duration::minutes(cfg.auth.access_ttl_minutes),
        refresh_ttl: time::Duration::days(cfg.auth.refresh_ttl_days),
        control_plane_url: cfg.control_plane_url,
        sync_url: cfg.sync_url,
        challenge_solver_url: cfg.challenge_solver_url,
        bus,
        http: reqwest::Client::new(),
        audit,
        cookie_secure: cfg.auth.cookie_secure,
        mailer,
        email_base_url: cfg.email.base_url,
    };

    // Readiness reflects what the edge actually needs to serve: Postgres is required, and
    // NATS is not (its absence only disables the live stream, which already degrades).
    let health = Health::builder().check(PostgresCheck::new(pool)).build();

    // Serve the metrics scrape on its own port when configured, keeping it off the
    // request-facing listener.
    tankovault_service::spawn_metrics_server(metrics.clone(), shutdown.clone());

    let app = tankovault_api::build_router(
        state,
        &cfg.security,
        &cfg.rate_limit,
        metrics,
        health,
        redis,
    );

    tankovault_service::serve(&cfg.bind_addr, app, shutdown).await?;
    Ok(())
}

/// The audit sink named by configuration.
///
/// Returned as a trait object so the toggle is resolved exactly once, here, and never
/// again at a call site.
fn build_audit_sink(
    pool: &tankovault_db::PgPool,
    cfg: &tankovault_config::AuditConfig,
) -> Arc<dyn tankovault_service::AuditSink> {
    if cfg.enabled {
        tracing::info!(
            record_ip = cfg.record_ip,
            record_user_agent = cfg.record_user_agent,
            retention_days = cfg.retention_days,
            "audit trail enabled"
        );
        Arc::new(PostgresAuditSink::new(pool.clone(), cfg))
    } else {
        tracing::warn!(
            "audit trail disabled by configuration; privileged actions are not recorded"
        );
        Arc::new(tankovault_service::NoopAuditSink)
    }
}

/// Start the audit-retention sweep, unless retention is switched off.
///
/// Runs on the API rather than a dedicated job because this is where the audit sink lives;
/// with several replicas each sweep is idempotent (a bounded `DELETE` by age), so no leader
/// election is needed — concurrent sweeps simply share the work.
fn spawn_audit_retention(
    pool: &tankovault_db::PgPool,
    cfg: &tankovault_config::AuditConfig,
    shutdown: tokio_util::sync::CancellationToken,
) {
    if !cfg.retention_enabled() {
        return;
    }
    let pool = pool.clone();
    let retention_days = cfg.retention_days;
    let interval = Duration::from_secs(cfg.sweep_interval_hours.max(1) * 3600);

    tracing::info!(
        retention_days,
        sweep_interval_hours = cfg.sweep_interval_hours,
        "audit retention sweep scheduled"
    );

    tokio::spawn(async move {
        tankovault_service::shutdown::every(interval, shutdown, "audit-retention", move || {
            let pool = pool.clone();
            async move {
                match tankovault_db::repo::audit::prune_older_than(
                    &pool,
                    retention_days,
                    AUDIT_PRUNE_BATCH,
                )
                .await
                {
                    Ok(0) => {}
                    Ok(deleted) => tracing::info!(deleted, retention_days, "pruned audit records"),
                    Err(e) => tracing::warn!(error = %e, "audit retention sweep failed"),
                }
            }
        })
        .await;
    });
}

/// Connect Redis for shared rate-limit counters, or `None` when unconfigured/unreachable.
async fn connect_redis(
    cfg: Option<&tankovault_config::RedisConfig>,
) -> Option<tankovault_service::ratelimit::RedisStoreHandle> {
    let cfg = cfg?;
    match fred::prelude::Builder::from_config(fred::prelude::Config::from_url(&cfg.url).ok()?)
        .build()
    {
        Ok(client) => match fred::prelude::ClientLike::init(&client).await {
            Ok(_) => {
                tracing::info!("connected to redis for shared rate-limit counters");
                Some(tankovault_service::ratelimit::RedisStoreHandle::new(client))
            }
            Err(e) => {
                tracing::warn!(error = %e, "redis unreachable; rate limits stay per replica");
                None
            }
        },
        Err(e) => {
            tracing::warn!(error = %e, "invalid redis configuration; rate limits stay per replica");
            None
        }
    }
}
