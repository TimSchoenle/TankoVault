//! # api service
//!
//! The public edge (design §11): Axum REST + JSON, JWT auth with rotating refresh
//! cookies, permission-gated admin routes, and link resolution at read time. This binary is a
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
    /// Runtime feature flags. Only the refresh cadence is configured here — which features
    /// are on is an operator decision made in the control plane at runtime.
    #[serde(default)]
    features: tankovault_config::FeaturesConfig,
    /// Shared secret presented to `sync`, `control-plane` and `challenge-solver`. Must be
    /// identical on every service in the internal tier.
    #[serde(default)]
    internal: tankovault_config::InternalAuthConfig,
}

#[derive(Debug, serde::Deserialize)]
struct AuthConfig {
    jwt_secret: String,
    /// Server-side password pepper: a secret mixed into every argon2id hash so a database
    /// leak alone cannot be brute-forced offline. Optional — empty (the default) keeps
    /// hashing un-peppered, which is backward-compatible with hashes stored before it was
    /// set. Once configured it must stay stable, or existing passwords stop verifying.
    #[serde(default)]
    password_pepper: String,
    #[serde(default = "default_access_minutes")]
    access_ttl_minutes: i64,
    #[serde(default = "default_refresh_days")]
    refresh_ttl_days: i64,
    /// Mark the refresh cookie `Secure`.
    ///
    /// Defaults to **true**. It was `#[serde(default)]` on a `bool` — that is `false` — and
    /// nothing in the reference deployment set it, so the shipped stack sent a 30-day
    /// credential over plain HTTP. One accidental `http://` on an untrusted network (a typo,
    /// an old bookmark, a captive portal, SSL-strip) handed it over; with HSTS also off by
    /// default, the browser had no memory that the origin should have been HTTPS.
    ///
    /// The opt-out exists for local HTTP development, where a `Secure` cookie is simply never
    /// sent. Set it explicitly there; do not default it off for everyone.
    ///
    /// It also selects the cookie's **name and path**: `__Host-refresh_token` at `Path=/` when
    /// on, the unprefixed `refresh_token` at `/v1/auth` when off, because a `__Host-` cookie
    /// without `Secure` is refused by the browser rather than downgraded. Flipping this setting
    /// therefore invalidates every already-issued refresh cookie — one forced sign-in, once. See
    /// `auth::session::refresh_cookie` for the review behind the wider path.
    #[serde(default = "tankovault_config::default_true")]
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

/// Minimum accepted length of `jwt_secret` in a production profile.
///
/// HS256 signs access tokens with this secret as the HMAC key; anything materially shorter than
/// the 256-bit output is a weak key an attacker can attempt to brute-force offline, and an empty
/// one forges tokens outright. 32 bytes is the floor a real deployment must clear.
const MIN_JWT_SECRET_LEN: usize = 32;

/// Whether this process is running under the production profile.
///
/// Keyed off `TANKOVAULT_PROFILE=production` (or `prod`). Absent/anything else is treated as
/// development, so local runs, tests and the integration harness — which use generated or short
/// secrets — are never blocked; only a real deployment opts into the strict check.
fn is_production() -> bool {
    matches!(
        std::env::var("TANKOVAULT_PROFILE").as_deref(),
        Ok("production" | "prod")
    )
}

/// Placeholder secrets that ship in this repository, and are therefore public.
///
/// Kept as data rather than as a `matches!` inside one caller: the same list has to be
/// consulted by the seed step and by any future secret. Returns the human name of the match
/// so the refusal can say *which* placeholder was found.
const KNOWN_PLACEHOLDERS: [(&str, &str); 4] = [
    ("dev-jwt-secret-change-me", "development JWT secret"),
    (
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        "all-zero token encryption key",
    ),
    ("changeme12345", "default seed administrator password"),
    ("change-me", "default email password"),
];

/// The name of the placeholder `value` matches, if any.
fn known_placeholder(value: &str) -> Option<&'static str> {
    let trimmed = value.trim();
    KNOWN_PLACEHOLDERS
        .iter()
        .find(|(placeholder, _)| *placeholder == trimmed)
        .map(|(_, name)| *name)
}

/// Fail fast on a misconfigured secret **before** the edge accepts a single request.
///
/// A production deployment that boots with a missing or weak `jwt_secret` can have every session
/// forged; refusing to start is strictly safer than serving with a broken trust root. An empty
/// `password_pepper` is a weakened — not broken — posture (hashes are simply un-peppered), so it
/// is a loud warning rather than a hard stop, preserving compatibility with hashes stored before
/// a pepper was configured.
///
/// # Errors
/// Returns an error in a production profile when `jwt_secret` is empty or shorter than
/// [`MIN_JWT_SECRET_LEN`].
fn validate_auth_secrets(auth: &AuthConfig, production: bool) -> anyhow::Result<()> {
    // Checked in **every** profile, not just production. A weak-secret check a deployment
    // can skip by forgetting one environment variable is not a check — and these exact
    // strings shipped in the reference compose file, so they are what an operator who never
    // set TANKOVAULT_PROFILE is running with.
    if let Some(name) = known_placeholder(&auth.jwt_secret) {
        anyhow::bail!(
            "refusing to start: jwt_secret is the well-known {name} placeholder, which is \
             published in this repository. Every session against it is forgeable by anyone \
             who has read deploy/docker-compose.yml. Set TANKOVAULT_AUTH__JWT_SECRET."
        );
    }

    if !production {
        if auth.jwt_secret.len() < MIN_JWT_SECRET_LEN {
            tracing::warn!(
                "jwt_secret is short ({} < {MIN_JWT_SECRET_LEN}); acceptable for development but \
                 set a strong secret and TANKOVAULT_PROFILE=production before deploying",
                auth.jwt_secret.len()
            );
        }
        return Ok(());
    }

    if auth.jwt_secret.trim().is_empty() {
        anyhow::bail!(
            "refusing to start: jwt_secret is empty in a production profile; every session could \
             be forged. Set a strong random secret (at least {MIN_JWT_SECRET_LEN} bytes)."
        );
    }
    if auth.jwt_secret.len() < MIN_JWT_SECRET_LEN {
        anyhow::bail!(
            "refusing to start: jwt_secret is too short ({} < {MIN_JWT_SECRET_LEN}) for a \
             production profile; it is brute-forceable. Set a strong random secret.",
            auth.jwt_secret.len()
        );
    }
    if auth.password_pepper.is_empty() {
        tracing::warn!(
            "password_pepper is empty in a production profile; password hashes are un-peppered, so \
             a database leak alone can be brute-forced offline. Configure a stable pepper."
        );
    }
    Ok(())
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

    // Refuse to boot with a broken trust root: a production deployment must not serve a single
    // request against a missing or brute-forceable JWT secret.
    validate_auth_secrets(&cfg.auth, is_production())?;
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

    // Likewise Redis: it sharpens rate limiting across replicas and holds the SSE stream
    // tickets, so an outage downgrades both to per-process state instead of refusing to start.
    let redis = connect_redis(cfg.redis.as_ref()).await;
    let stream_tickets = stream_ticket_store(redis.as_ref());

    // Build the transactional email back-end. A missing/invalid relay degrades to a no-op
    // mailer (logs and drops) so the edge still boots and login/registration keep working.
    let mailer = tankovault_email::build(&cfg.email);

    let audit = build_audit_sink(&pool, &cfg.audit);
    spawn_audit_retention(&pool, &cfg.audit, shutdown.clone());

    // Awaited: the listener must not accept a request until the gate reflects the operator's
    // stored decisions, or a restart would briefly re-enable everything switched off.
    let features =
        tankovault_api::install_feature_gate(pool.clone(), &cfg.features, shutdown.clone()).await;

    // One client for every internal hop, with connect and request timeouts. The previous
    // `reqwest::Client::new()` had neither, and fed an unbounded `tokio::spawn` in
    // `spawn_targeted_push` — a hung `sync` leaked a task and a socket per marked chapter.
    let internal_http = tankovault_api::Upstream::client()?;
    let internal_token = tankovault_service::internal_auth::resolve(&cfg.internal)?;

    let state = AppState {
        pool: pool.clone(),
        jwt_secret: Arc::new(cfg.auth.jwt_secret.into_bytes()),
        password_pepper: Arc::new(cfg.auth.password_pepper.into_bytes()),
        access_ttl: time::Duration::minutes(cfg.auth.access_ttl_minutes),
        refresh_ttl: time::Duration::days(cfg.auth.refresh_ttl_days),
        control_plane: tankovault_api::Upstream::new(
            internal_http.clone(),
            cfg.control_plane_url,
            internal_token.clone(),
            "control-plane",
        ),
        sync: tankovault_api::Upstream::new(
            internal_http,
            cfg.sync_url,
            internal_token.clone(),
            "sync",
        ),
        challenge_solver_url: cfg.challenge_solver_url,
        internal_token,
        bus,
        stream_tickets,
        audit,
        features,
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
        redis.map(tankovault_service::ratelimit::RedisStoreHandle::new),
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

/// Connect Redis, or `None` when unconfigured/unreachable.
///
/// One client for both of this service's Redis users — the cross-replica rate-limit counters and
/// the SSE stream-ticket store — as `tankovault_service::ratelimit::redis` already assumes ("the
/// same connection is typically shared with other Redis users in the process"). Neither is worth
/// refusing to boot over: rate limits degrade to per-replica counters and tickets to per-process
/// ones, both with a warning.
async fn connect_redis(
    cfg: Option<&tankovault_config::RedisConfig>,
) -> Option<fred::clients::Client> {
    let cfg = cfg?;
    match fred::prelude::Builder::from_config(fred::prelude::Config::from_url(&cfg.url).ok()?)
        .build()
    {
        Ok(client) => match fred::prelude::ClientLike::init(&client).await {
            Ok(_) => {
                tracing::info!("connected to redis for rate-limit counters and stream tickets");
                Some(client)
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "redis unreachable; rate limits stay per replica and stream tickets per process"
                );
                None
            }
        },
        Err(e) => {
            tracing::warn!(
                error = %e,
                "invalid redis configuration; rate limits stay per replica and stream tickets per process"
            );
            None
        }
    }
}

/// The stream-ticket store this deployment gets.
///
/// Redis where it is available, this process otherwise. The fallback is **wrong across
/// replicas** — a ticket minted on one is unknown to the others, so opening the stream fails
/// until a retry happens to land on the minting replica — and it says so loudly rather than
/// degrading in silence. Refusing to boot instead would take the whole edge down for a
/// best-effort notification badge.
fn stream_ticket_store(
    redis: Option<&fred::clients::Client>,
) -> std::sync::Arc<dyn tankovault_api::stream_tickets::StreamTicketStore> {
    if let Some(client) = redis {
        return std::sync::Arc::new(tankovault_api::stream_tickets::RedisStreamTickets::new(
            client.clone(),
        ));
    }
    tracing::warn!(
        "no redis: SSE stream tickets are per-process, so /v1/me/stream will fail to open \
         behind more than one api replica"
    );
    std::sync::Arc::new(tankovault_api::stream_tickets::MemoryStreamTickets::new())
}

#[cfg(test)]
mod tests {
    use super::{AuthConfig, MIN_JWT_SECRET_LEN, validate_auth_secrets};

    fn auth(jwt_secret: &str, pepper: &str) -> AuthConfig {
        AuthConfig {
            jwt_secret: jwt_secret.to_owned(),
            password_pepper: pepper.to_owned(),
            access_ttl_minutes: 15,
            refresh_ttl_days: 30,
            cookie_secure: true,
        }
    }

    /// The published placeholder must be refused **outside** production too. A check an
    /// operator can skip by forgetting `TANKOVAULT_PROFILE` is not a check, and this exact
    /// string shipped in `deploy/docker-compose.yml`.
    #[test]
    fn the_published_placeholder_secret_is_refused_in_every_profile() {
        for production in [true, false] {
            let err =
                validate_auth_secrets(&auth("dev-jwt-secret-change-me", "pepper"), production)
                    .unwrap_err();
            assert!(
                err.to_string().contains("placeholder"),
                "production={production}, got: {err}"
            );
        }
        // Surrounding whitespace must not smuggle it past the comparison.
        assert!(
            validate_auth_secrets(&auth("  dev-jwt-secret-change-me  ", "pepper"), false).is_err()
        );
    }

    #[test]
    fn known_placeholders_are_named_in_the_refusal() {
        assert_eq!(
            super::known_placeholder("changeme12345"),
            Some("default seed administrator password")
        );
        assert_eq!(
            super::known_placeholder("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="),
            Some("all-zero token encryption key")
        );
        assert_eq!(super::known_placeholder("a-real-looking-secret"), None);
    }

    #[test]
    fn production_rejects_an_empty_secret() {
        let err = validate_auth_secrets(&auth("", "pepper"), true).unwrap_err();
        assert!(err.to_string().contains("empty"), "got: {err}");
    }

    #[test]
    fn production_rejects_a_short_secret() {
        let short = "x".repeat(MIN_JWT_SECRET_LEN - 1);
        let err = validate_auth_secrets(&auth(&short, "pepper"), true).unwrap_err();
        assert!(err.to_string().contains("too short"), "got: {err}");
    }

    #[test]
    fn production_accepts_a_strong_secret() {
        let strong = "x".repeat(MIN_JWT_SECRET_LEN);
        assert!(validate_auth_secrets(&auth(&strong, "pepper"), true).is_ok());
    }

    #[test]
    fn production_tolerates_an_empty_pepper() {
        // An empty pepper is weaker, not broken: it must warn, never block a boot.
        let strong = "x".repeat(MIN_JWT_SECRET_LEN);
        assert!(validate_auth_secrets(&auth(&strong, ""), true).is_ok());
    }

    #[test]
    fn development_never_blocks_even_an_empty_secret() {
        // Local runs, tests and the integration harness must boot with whatever they have.
        assert!(validate_auth_secrets(&auth("", ""), false).is_ok());
        assert!(validate_auth_secrets(&auth("short", ""), false).is_ok());
    }
}
