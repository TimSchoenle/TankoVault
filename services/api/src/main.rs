//! Entrypoint for the `api` service: loads config, wires up infra, and calls into
//! `tankovault_api` for the route table and app state.

use secrecy::{ExposeSecret as _, SecretSlice, SecretString};
use std::sync::Arc;
use std::time::Duration;
use tankovault_api::AppState;
use tankovault_service::{
    CancellationToken, Health, MetricsRegistry, PostgresAuditSink, health::PostgresCheck,
};

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
    #[serde(default = "default_worker")]
    worker_url: String,
    /// NATS for the live SSE relay; absent or unreachable only degrades `/v1/me/stream`.
    #[serde(default)]
    nats: Option<tankovault_config::NatsConfig>,
    /// Redis for cross-replica rate-limit counters; falls back to per-replica in-memory without it.
    #[serde(default)]
    redis: Option<tankovault_config::RedisConfig>,
    /// Transactional email (registration, password reset); unconfigured falls back to a no-op mailer.
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
    /// Runtime feature flags; only refresh cadence lives here — on/off is an operator
    /// decision made in the control plane at runtime.
    #[serde(default)]
    features: tankovault_config::FeaturesConfig,
    /// Shared secret presented to `sync`, `control-plane` and `challenge-solver`; must match
    /// across every service in the internal tier.
    #[serde(default)]
    internal: tankovault_config::InternalAuthConfig,
    /// Operator-published legal documents (Terms, Data Policy, Imprint, ...). An absent
    /// section publishes nothing, which is a valid deployment.
    #[serde(default)]
    legal: tankovault_config::LegalConfig,
    /// What this deployment calls itself: name, wordmark, copyright, links. Served to the
    /// client at `/v1/branding` and stamped into email and the authenticator prompts.
    #[serde(default)]
    branding: tankovault_config::BrandingConfig,
    /// Which repository the native client updates from, and the client versions this
    /// deployment supports. Served at `/v1/client`; an absent section publishes the upstream
    /// channel with this build's own version as the ceiling.
    #[serde(default)]
    client: tankovault_config::ClientConfig,
    /// Metadata intake rules. The API writes no metadata; it reads this section for the adult
    /// classifier alone, and shares it with the worker so the genres the public tag facet
    /// withholds are exactly the ones that put a series behind the gate.
    #[serde(default)]
    metadata: tankovault_config::MetadataPriorityConfig,
}

#[derive(Debug, serde::Deserialize)]
struct AuthConfig {
    /// HS256 signing key for access tokens; wrapped in `SecretString` so a `tracing::debug!(?cfg)`
    /// on this Debug-deriving, nested struct can't publish the key that authenticates every session.
    jwt_secret: SecretString,
    /// Server-side password pepper: mixed into every argon2id hash so a leak alone can't be
    /// brute-forced offline. Empty (default) is un-peppered, for compatibility with old
    /// hashes; once configured it must stay stable or passwords stop verifying.
    #[serde(default)]
    password_pepper: SecretString,
    #[serde(default = "default_access_minutes")]
    access_ttl_minutes: i64,
    #[serde(default = "default_refresh_days")]
    refresh_ttl_days: i64,
    /// Mark the refresh cookie `Secure`.
    ///
    /// Defaults to true — a `bool`'s implicit default is `false`, which would silently send a
    /// 30-day refresh credential over plain HTTP.
    ///
    /// Also selects the cookie's name/path (`__Host-` vs unprefixed), since a `__Host-`
    /// cookie without `Secure` is refused by the browser. Flipping this forces one re-login.
    #[serde(default = "tankovault_config::default_true")]
    cookie_secure: bool,
    /// Public origin of the web app, for passkeys — `https://tanko.example.com`.
    ///
    /// Cannot be inferred from a request: `Host` is attacker-controlled, and trusting it
    /// would let anyone mint credentials under a domain of their choosing. Unset falls back
    /// to [`tankovault_config::EmailConfig::base_url`] and disables only passkeys.
    #[serde(default)]
    webauthn_origin: Option<String>,
    /// Relying-party id: the registrable domain credentials are bound to. Defaults to
    /// [`Self::webauthn_origin`]'s host. Set to a parent domain only if the app moves between
    /// subdomains and keys must survive the move.
    #[serde(default)]
    webauthn_rp_id: Option<String>,
    /// The name the authenticator shows in its prompt ("Save a passkey for …"); purely cosmetic.
    #[serde(default)]
    webauthn_rp_name: Option<String>,
    /// Base64 (standard alphabet) 32-byte key that seals TOTP secrets at rest.
    ///
    /// Deliberately *not* derived from [`Self::jwt_secret`]: rotating the signing key is a
    /// routine operation, and deriving from it would silently invalidate every enrolled
    /// authenticator app — the same class of failure as rotating `password_pepper` and
    /// stranding the seeded admin. Unset disables TOTP enrolment only; security keys and
    /// recovery codes are unaffected, since neither stores a symmetric secret.
    #[serde(default)]
    mfa_encryption_key: Option<SecretString>,
    /// The issuer name an authenticator app files the entry under. Defaults to
    /// [`Self::webauthn_rp_name`], then to the product name — one label for both prompts.
    #[serde(default)]
    totp_issuer: Option<String>,
    /// How long a step-up elevation survives **unused** before a sensitive action prompts again.
    /// Every elevated request slides it forward.
    #[serde(default = "default_step_up_minutes")]
    step_up_ttl_minutes: i64,
    /// The ceiling on that sliding: the longest an elevation is honoured after it was earned,
    /// however continuously it is used.
    #[serde(default = "default_step_up_max_minutes")]
    step_up_max_ttl_minutes: i64,
    /// How long a half-finished sign-in — password accepted, second factor still owed — may
    /// sit before the user has to start again.
    #[serde(default = "default_mfa_challenge_minutes")]
    mfa_challenge_ttl_minutes: i64,
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
/// The worker's ops listener, which also serves the internally-authenticated dry-run.
///
/// Port 8085 is the worker's own default; either compose replica may answer since a dry
/// run is stateless.
fn default_worker() -> String {
    "http://worker:8085".to_owned()
}
fn default_access_minutes() -> i64 {
    15
}
fn default_refresh_days() -> i64 {
    30
}
/// Thirty minutes of *inactivity*. The window slides on every elevated request, so this is how
/// long an operator may leave a console panel alone before it asks again — five minutes measured
/// from the confirmation meant a re-prompt in the middle of the very task it was earned for.
fn default_step_up_minutes() -> i64 {
    30
}
/// Four hours, and nothing extends it: this is the bound a shift at the console cannot slide
/// past, so a walked-away-from machine stops being elevated even if something on the page keeps
/// touching a guarded route.
fn default_step_up_max_minutes() -> i64 {
    240
}
/// Five minutes, matching the `WebAuthn` ceremony timeout — a sign-in that offers a security
/// key must not expire its own challenge before the authenticator prompt does.
fn default_mfa_challenge_minutes() -> i64 {
    5
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
/// Keyed off `TANKOVAULT_PROFILE=production`/`prod`; anything else is development, so tests
/// and the integration harness with short secrets are never blocked.
fn is_production() -> bool {
    matches!(
        std::env::var("TANKOVAULT_PROFILE").as_deref(),
        Ok("production" | "prod")
    )
}

/// Placeholder secrets that ship in this repository, and are therefore public.
///
/// Kept as data rather than a `matches!` inside one caller, since the seed step consults
/// the same list.
const KNOWN_PLACEHOLDERS: [(&str, &str); 4] = [
    ("dev-jwt-secret-change-me", "development JWT secret"),
    (
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        "all-zero token encryption key",
    ),
    ("changeme12345", "default seed administrator password"),
    ("change-me", "default email password"),
];

/// Re-wrap a configured secret as the key material `tankovault_auth` takes.
///
/// Doing this crossing here, once, for both secrets means neither the JWT key nor the
/// pepper ever exists as a bare `String` or `Vec<u8>` in this process.
fn secret_bytes(value: &SecretString) -> SecretSlice<u8> {
    SecretSlice::from(value.expose_secret().as_bytes().to_vec())
}

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
/// A weak or missing `jwt_secret` in production can have every session forged, so refusing to
/// start is safer than serving with a broken trust root. An empty `password_pepper` is only
/// weakened, not broken, so it warns instead of blocking boot.
///
/// # Errors
/// Production only: `jwt_secret` empty or shorter than [`MIN_JWT_SECRET_LEN`].
fn validate_auth_secrets(auth: &AuthConfig, production: bool) -> anyhow::Result<()> {
    // Checked in every profile — placeholder strings ship in the reference compose file.
    // Length is safe to log; the value never is (`expose_secret` only measures/compares).
    if let Some(name) = known_placeholder(auth.jwt_secret.expose_secret()) {
        anyhow::bail!(
            "refusing to start: jwt_secret is the well-known {name} placeholder, which is \
             published in this repository. Every session against it is forgeable by anyone \
             who has read deploy/docker-compose.yml. Set TANKOVAULT_AUTH__JWT_SECRET."
        );
    }

    if !production {
        if auth.jwt_secret.expose_secret().len() < MIN_JWT_SECRET_LEN {
            tracing::warn!(
                "jwt_secret is short ({} < {MIN_JWT_SECRET_LEN}); acceptable for development but \
                 set a strong secret and TANKOVAULT_PROFILE=production before deploying",
                auth.jwt_secret.expose_secret().len()
            );
        }
        return Ok(());
    }

    if auth.jwt_secret.expose_secret().trim().is_empty() {
        anyhow::bail!(
            "refusing to start: jwt_secret is empty in a production profile; every session could \
             be forged. Set a strong random secret (at least {MIN_JWT_SECRET_LEN} bytes)."
        );
    }
    if auth.jwt_secret.expose_secret().len() < MIN_JWT_SECRET_LEN {
        anyhow::bail!(
            "refusing to start: jwt_secret is too short ({} < {MIN_JWT_SECRET_LEN}) for a \
             production profile; it is brute-forceable. Set a strong random secret.",
            auth.jwt_secret.expose_secret().len()
        );
    }
    if auth.password_pepper.expose_secret().is_empty() {
        tracing::warn!(
            "password_pepper is empty in a production profile; password hashes are un-peppered, so \
             a database leak alone can be brute-forced offline. Configure a stable pepper."
        );
    }
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // First, before anything can build a rustls configuration: rustls cannot choose a provider
    // for itself in this graph and panics instead of erroring. See `tankovault_service::crypto`.
    tankovault_service::install_crypto_provider();

    // Before anything else: this may be Docker's HEALTHCHECK invocation, not the service.
    // `scratch` images have no shell, so the binary probing itself is the only probe available.
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
    // Serve the metrics scrape on its own port when configured, keeping it off the
    // request-facing listener. Outside the reloadable runtime so a reload does not rebind it.
    tankovault_service::spawn_metrics_server(metrics.clone(), shutdown.clone());

    // Boxed, not because the future is held across an await here — it is the whole program —
    // but because it carries the config by value, and this one is among the largest in the
    // workspace. Left on the stack it trips `clippy::large_futures`, which is a stack-overflow
    // guard rather than a style rule.
    Box::pin(tankovault_service::run_reloading(
        boot,
        &shutdown,
        |cfg, generation| serve_once(cfg, metrics.clone(), generation),
    ))
    .await
}

/// Build and run everything a configuration change rebuilds: the pool, the broker and Redis
/// connections, the mailer, the audit sink, the application state, the router and the listener.
///
/// Returns when `shutdown` is cancelled — by the OS signal, or by the supervisor because the
/// configuration changed and this runtime is being replaced.
///
/// The secret validation runs here rather than in `main` deliberately: it has to hold for a
/// *rotated* secret too, and a rotation that fails it leaves the previous runtime serving
/// rather than taking the pod down. Note what a successful `auth.jwt_secret` rotation means —
/// every session signed with the old key stops verifying, so every user is signed out. That is
/// the correct behaviour for a compromised key and a surprising one otherwise.
/// What this service needs to *call* the internal tier.
struct Internal {
    auth: tankovault_config::ResolvedInternalAuth,
    /// Presented under `identity = "token"`.
    token: Option<tankovault_service::InternalToken>,
    /// Presented under `identity = "mtls"`.
    tls: Option<tankovault_service::ClientMaterial>,
}

/// Resolve and cross-check the internal-tier identity.
///
/// The API serves no internally-authenticated route of its own — it is a caller only — so what
/// it takes from the `internal` section is whichever outbound credential the mode uses: a token
/// its peers list under `api`, or a client certificate they match by SAN.
///
/// Called before anything connects. Every check here is pure configuration, and a deployment
/// that is going to be refused should be refused before it opens a socket.
fn resolve_internal(cfg: &Config) -> anyhow::Result<Internal> {
    let auth = tankovault_service::internal_auth::resolve(&cfg.internal)?;

    // Checked here, where the URLs are, rather than inside the client: an `http://` upstream
    // under mtls is the one misconfiguration that produces no symptom at all.
    for (name, url) in [
        ("control-plane", &cfg.control_plane_url),
        ("sync", &cfg.sync_url),
        ("worker", &cfg.worker_url),
    ] {
        tankovault_service::internal_auth::check_upstream_scheme(auth.mode, name, url)?;
    }

    let token = auth
        .caller
        .as_ref()
        .and_then(|c| c.token.clone())
        .map(tankovault_service::InternalToken::new);
    let tls = auth
        .tls
        .as_ref()
        .map(tankovault_service::client_material)
        .transpose()?;

    Ok(Internal { auth, token, tls })
}

async fn serve_once(
    cfg: Arc<Config>,
    metrics: MetricsRegistry,
    shutdown: CancellationToken,
) -> anyhow::Result<()> {
    // Refuse to boot with a broken trust root: a production deployment must not serve a single
    // request against a missing or brute-forceable JWT secret.
    validate_auth_secrets(&cfg.auth, is_production())?;
    // A legal document with no file and no URL would 404 on a link the footer publishes from
    // the same config that omitted it. Refusing to boot names the slug, which is the fix.
    cfg.legal.validate()?;

    let pool = tankovault_db::connect(
        &cfg.database.url,
        cfg.database.max_connections,
        cfg.database.acquire_timeout_secs,
    )
    .await?;
    tankovault_service::metrics::spawn_pool_sampler(pool.clone(), shutdown.clone());

    // Awaited before anything serves: a deployment with no super user has no account that
    // outlives the next capability the codebase gains, and nothing in the API can mint the grant
    // afterwards.
    tankovault_api::ensure_deployment_owner(&pool).await;

    let Internal {
        auth: internal_auth,
        token: internal_token,
        tls: internal_tls,
    } = resolve_internal(&cfg)?;

    // Connect to NATS for the live SSE relay. A broker outage must not stop the public edge
    // from booting, so a failure here degrades the feature to `503` rather than aborting.
    let bus = tankovault_api::connect_bus(cfg.nats.as_ref(), internal_auth.tls.as_ref()).await;

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
    // Same timer, and awaited for the same reason: a shelf built from compiled defaults while
    // the operator's tuning sits unread is a behaviour change nobody asked for.
    let tunables =
        tankovault_api::install_tunables(pool.clone(), &cfg.features, shutdown.clone()).await;

    // One client for every internal hop, with connect and request timeouts — without them, a
    // hung downstream leaks a task and a socket per request.
    let internal_http = tankovault_api::Upstream::client(internal_tls.as_ref())?;
    let internal_http_worker = internal_http.clone();

    // `None` origin is a valid "no passkeys" state; a *malformed* one is fatal — it would
    // otherwise surface as browsers refusing every ceremony with an opaque `SecurityError`.
    let webauthn = build_relying_party(&cfg.auth, &cfg.email.base_url, &cfg.branding.name)?;
    let mfa_sealer = build_mfa_sealer(&cfg.auth)?;

    // Fatal for the same reason the origin above is: a client that cannot read the ceiling
    // behaves as though there were none, so an unusable value would quietly undo the version
    // range rather than report it. The service's own version is the default ceiling — client and
    // server are cut from this repository at one version.
    let client_channel = tankovault_api::ClientChannel::new(&cfg.client, env!("CARGO_PKG_VERSION"))
        .map_err(|error| anyhow::anyhow!(error))?;

    // Abandoned ceremonies — a user who closed the tab at the authenticator prompt — are
    // already unusable, so this reclaims rows rather than enforcing anything. The same sweep
    // clears expired sign-in challenges and step-up grants, which have the same shape.
    spawn_credential_sweep(&pool, shutdown.clone());

    let state = AppState {
        pool: pool.clone(),
        // The only place these cross from text to key material; `Arc` because axum clones
        // state per request — see `AppState::jwt_secret`.
        jwt_secret: Arc::new(secret_bytes(&cfg.auth.jwt_secret)),
        password_pepper: Arc::new(secret_bytes(&cfg.auth.password_pepper)),
        access_ttl: time::Duration::minutes(cfg.auth.access_ttl_minutes),
        refresh_ttl: time::Duration::days(cfg.auth.refresh_ttl_days),
        control_plane: tankovault_api::Upstream::new(
            internal_http.clone(),
            cfg.control_plane_url.clone(),
            internal_token.clone(),
            "control-plane",
        ),
        sync: tankovault_api::Upstream::new(
            internal_http.clone(),
            cfg.sync_url.clone(),
            internal_token.clone(),
            "sync",
        ),
        worker: tankovault_api::Upstream::new(
            internal_http_worker,
            cfg.worker_url.clone(),
            internal_token,
            "worker",
        ),
        bus,
        stream_tickets,
        audit,
        features,
        tunables,
        cookie_secure: cfg.auth.cookie_secure,
        totp_issuer: cfg
            .auth
            .totp_issuer
            .clone()
            .or_else(|| cfg.auth.webauthn_rp_name.clone())
            .unwrap_or_else(|| cfg.branding.name.clone()),
        step_up_ttl: time::Duration::minutes(cfg.auth.step_up_ttl_minutes),
        step_up_max_ttl: time::Duration::minutes(cfg.auth.step_up_max_ttl_minutes),
        mfa_challenge_ttl: time::Duration::minutes(cfg.auth.mfa_challenge_ttl_minutes),
        webauthn,
        mfa_sealer,
        mailer,
        email_base_url: cfg.email.base_url.clone(),
        legal: tankovault_api::LegalDocs::new(cfg.legal.clone()),
        branding: tankovault_api::Branding::new(cfg.branding.clone()),
        client_channel,
        system_stats: tankovault_api::Cached::new(tankovault_api::ADMIN_STATS_TTL),
        provider_stats: tankovault_api::Cached::new(tankovault_api::ADMIN_STATS_TTL),
        adult_tags: Arc::new(cfg.metadata.tags.adult_tags()),
    };

    // Readiness reflects what the edge actually needs to serve: Postgres is required, and
    // NATS is not (its absence only disables the live stream, which already degrades).
    let health = Health::builder().check(PostgresCheck::new(pool)).build();

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

/// The audit sink named by configuration; returned as a trait object so the toggle is
/// resolved once, here.
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
/// Idempotent (a bounded `DELETE` by age), so replicas racing on it just share the work —
/// no leader election needed.
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

/// The `WebAuthn` relying party this deployment offers, if any.
///
/// Falls back to `email.base_url`: the password-reset link resolves to the same origin a
/// passkey must be bound to, so duplicating the string risks silent disagreement.
///
/// # Errors
/// Origin is not a URL, has no host, or isn't covered by the relying-party id.
fn build_relying_party(
    auth: &AuthConfig,
    email_base_url: &str,
    brand: &str,
) -> anyhow::Result<Option<tankovault_api::SharedRelyingParty>> {
    let origin = auth
        .webauthn_origin
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(email_base_url);

    let rp = tankovault_api::RelyingParty::from_config(
        Some(origin),
        auth.webauthn_rp_id.as_deref(),
        auth.webauthn_rp_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or(brand),
    )?;

    let Some(rp) = rp else {
        tracing::warn!(
            "no webauthn origin resolved; passkeys are unavailable. Set \
             TANKOVAULT_AUTH__WEBAUTHN_ORIGIN (or TANKOVAULT_EMAIL__BASE_URL) to the \
             public origin of the web app."
        );
        return Ok(None);
    };

    tracing::info!(
        rp_id = rp.rp_id(),
        origin = rp.origin(),
        "passkeys enabled; credentials are bound to this origin"
    );
    Ok(Some(Arc::new(rp)))
}

/// Build the sealer that protects TOTP secrets at rest, or `None` when unconfigured.
///
/// Logged loudly rather than fatal, matching [`build_relying_party`]: TOTP is one factor of an
/// application that must still start without it, and security keys remain available. A bad key
/// *is* fatal, because it means the operator configured one and it will not work — silently
/// falling back to "TOTP disabled" would hide a typo behind a feature that quietly vanished.
///
/// # Errors
/// When the value is not base64 or does not decode to exactly 32 bytes.
fn build_mfa_sealer(auth: &AuthConfig) -> anyhow::Result<Option<tankovault_auth::Sealer>> {
    let Some(key) = auth
        .mfa_encryption_key
        .as_ref()
        .filter(|k| !k.expose_secret().trim().is_empty())
    else {
        tracing::warn!(
            "no auth.mfa_encryption_key configured; authenticator-app (TOTP) enrolment is \
             unavailable. Set TANKOVAULT_AUTH__MFA_ENCRYPTION_KEY to a base64-encoded 32-byte \
             key. Security keys and recovery codes are unaffected."
        );
        return Ok(None);
    };

    let sealer = tankovault_auth::Sealer::from_base64_key(key).map_err(|e| {
        anyhow::anyhow!(
            "auth.mfa_encryption_key is not usable: {e}. Expected base64 (standard alphabet) \
             of exactly 32 bytes, e.g. `openssl rand -base64 32`"
        )
    })?;
    tracing::info!("authenticator-app (TOTP) enrolment enabled; secrets are sealed at rest");
    Ok(Some(sealer))
}

/// How often abandoned credential state is swept.
///
/// Generous: expiry is already enforced in every read (`take_ceremony`,
/// `charge_challenge_attempt` and `renew_step_up` all filter on `expires_at`), so an unswept row
/// is unusable, not dangerous — this only stops three tables growing without bound.
const CREDENTIAL_SWEEP_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// Start the sweep over abandoned `WebAuthn` ceremonies, half-finished sign-ins and expired
/// step-up grants.
///
/// Runs unconditionally, unlike the passkey-only version it replaced: sign-in challenges and
/// step-up grants are written whenever a second factor is enrolled, which does not require a
/// relying party — an account with TOTP alone produces both. Idempotent like the audit sweep,
/// so racing replicas just share the work.
fn spawn_credential_sweep(
    pool: &tankovault_db::PgPool,
    shutdown: tokio_util::sync::CancellationToken,
) {
    let pool = pool.clone();
    tokio::spawn(async move {
        tankovault_service::shutdown::every(
            CREDENTIAL_SWEEP_INTERVAL,
            shutdown,
            "credential-state-sweep",
            move || {
                let pool = pool.clone();
                async move {
                    sweep_once(&pool).await;
                }
            },
        )
        .await;
    });
}

/// One pass of the credential sweep. Each table is independent: a failure on one is logged and
/// the others still run, because a sweep that gives up on the first error leaves the *other*
/// two growing for reasons nothing names.
async fn sweep_once(pool: &tankovault_db::PgPool) {
    use tankovault_db::repo::users::{mfa, webauthn};

    match webauthn::prune_expired_ceremonies(pool).await {
        Ok(0) => {}
        Ok(deleted) => tracing::debug!(deleted, "pruned abandoned webauthn ceremonies"),
        Err(e) => tracing::warn!(error = %e, "webauthn ceremony sweep failed"),
    }
    match mfa::prune_expired_challenges(pool).await {
        Ok(0) => {}
        Ok(deleted) => tracing::debug!(deleted, "pruned expired sign-in challenges"),
        Err(e) => tracing::warn!(error = %e, "sign-in challenge sweep failed"),
    }
    match mfa::prune_expired_step_ups(pool).await {
        Ok(0) => {}
        Ok(deleted) => tracing::debug!(deleted, "pruned expired step-up grants"),
        Err(e) => tracing::warn!(error = %e, "step-up grant sweep failed"),
    }
}

/// Connect Redis, or `None` when unconfigured/unreachable.
///
/// One client shared by both Redis users (rate-limit counters, stream tickets). Neither is
/// worth refusing to boot over — both degrade to per-replica/per-process state with a warning.
async fn connect_redis(
    cfg: Option<&tankovault_config::RedisConfig>,
) -> Option<fred::clients::Client> {
    let cfg = cfg?;
    match fred::prelude::Builder::from_config(
        fred::prelude::Config::from_url(cfg.url.expose_secret()).ok()?,
    )
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

/// The stream-ticket store this deployment gets: Redis where available, this process otherwise.
///
/// The fallback is wrong across replicas — a ticket minted on one is unknown to others — so
/// it warns loudly rather than degrading silently.
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
    use secrecy::SecretString;

    fn auth(jwt_secret: &str, pepper: &str) -> AuthConfig {
        AuthConfig {
            jwt_secret: SecretString::from(jwt_secret),
            password_pepper: SecretString::from(pepper),
            access_ttl_minutes: 15,
            refresh_ttl_days: 30,
            cookie_secure: true,
            webauthn_origin: None,
            webauthn_rp_id: None,
            webauthn_rp_name: None,
            mfa_encryption_key: None,
            totp_issuer: None,
            step_up_ttl_minutes: 5,
            step_up_max_ttl_minutes: 60,
            mfa_challenge_ttl_minutes: 5,
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
