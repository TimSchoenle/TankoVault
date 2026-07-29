//! # tankovault-config
//!
//! Layered, typed configuration. Precedence (low → high):
//! 1. Struct-level `#[serde(default)]` values.
//! 2. An optional TOML file at `$TANKOVAULT_CONFIG` (defaults to `config.toml` if present).
//! 3. Environment variables prefixed `TANKOVAULT_`, nested with `__`
//!    (`TANKOVAULT_DATABASE__MAX_CONNECTIONS=20`).
//!
//! Each service defines its own top-level config struct composed of the shared building
//! blocks here, then calls [`load`].

use figment::{
    Figment,
    providers::{Env, Format, Toml},
};
use serde::Deserialize;
use serde::de::DeserializeOwned;

/// Errors raised while assembling configuration.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// A figment extraction/merge failure (missing required key, type mismatch, ...).
    /// Boxed because `figment::Error` is large relative to a typical `Result`.
    #[error("configuration error: {0}")]
    Figment(#[from] Box<figment::Error>),
    /// A value parsed but is not usable — too short, or absent where the active profile
    /// requires it. Kept distinct from [`Self::Figment`] so the message can name the fix.
    #[error("configuration error: {0}")]
    Invalid(String),
}

/// Load a typed config for a service.
///
/// # Errors
/// Returns [`ConfigError`] if a required value is missing or a value fails to parse.
pub fn load<T: DeserializeOwned>() -> Result<T, ConfigError> {
    let path = std::env::var("TANKOVAULT_CONFIG").unwrap_or_else(|_| "config.toml".to_owned());
    let figment = Figment::new()
        .merge(Toml::file(path))
        .merge(Env::prefixed("TANKOVAULT_").split("__"));
    Ok(figment.extract().map_err(Box::new)?)
}

/// `PostgreSQL` connection settings.
#[derive(Debug, Clone, Deserialize)]
pub struct DatabaseConfig {
    /// e.g. `postgres://user:pass@host:5432/tankovault`.
    pub url: String,
    /// Upper bound on the connection pool.
    #[serde(default = "DatabaseConfig::default_max_connections")]
    pub max_connections: u32,
    /// Statement/acquire timeout, seconds.
    #[serde(default = "DatabaseConfig::default_acquire_timeout_secs")]
    pub acquire_timeout_secs: u64,
}

impl DatabaseConfig {
    fn default_max_connections() -> u32 {
        16
    }
    fn default_acquire_timeout_secs() -> u64 {
        10
    }
}

/// Redis connection settings (cache, rate-limit counters, locks, solved sessions).
#[derive(Debug, Clone, Deserialize)]
pub struct RedisConfig {
    /// e.g. `redis://redis:6379`.
    pub url: String,
}

/// NATS `JetStream` connection settings.
#[derive(Debug, Clone, Deserialize)]
pub struct NatsConfig {
    /// e.g. `nats://nats:4222`.
    pub url: String,
}

/// HTTP server binding for a service.
#[derive(Debug, Clone, Deserialize)]
pub struct HttpConfig {
    /// e.g. `0.0.0.0:8080`.
    #[serde(default = "HttpConfig::default_bind")]
    pub bind_addr: String,
}

impl HttpConfig {
    fn default_bind() -> String {
        "0.0.0.0:8080".to_owned()
    }
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            bind_addr: Self::default_bind(),
        }
    }
}

/// Observability / telemetry settings shared by every service.
#[derive(Debug, Clone, Deserialize)]
pub struct TelemetryConfig {
    /// Logical service name reported in traces/metrics.
    pub service_name: String,
    /// **Removed.** `TANKOVAULT_TELEMETRY__OTLP_ENDPOINT` used to be accepted here and did
    /// nothing but log "collector export is pending" — no `OTel` layer was ever installed and
    /// the four `OTel` crates in `[workspace.dependencies]` were used by zero members.
    ///
    /// A knob that silently does nothing is worse than an absent one: an operator who sets it
    /// believes traces are being exported and discovers otherwise during an incident. Setting
    /// the variable is now a hard configuration error (figment rejects unknown keys only when
    /// asked, so the removal is documented here instead), which is the honest signal.
    ///
    /// Re-add this together with a real `OpenTelemetryLayer` in
    /// `crates/service/src/telemetry.rs`, never separately.
    /// `RUST_LOG`-style filter (e.g. `info,tankovault=debug`).
    #[serde(default = "TelemetryConfig::default_log_filter")]
    pub log_filter: String,
    /// Emit structured JSON logs (production) vs. pretty logs (dev).
    #[serde(default)]
    pub json_logs: bool,
}

impl TelemetryConfig {
    fn default_log_filter() -> String {
        "info".to_owned()
    }
}

/// Transport security for an SMTP relay.
///
/// Chosen explicitly rather than inferred from the port so an operator's intent is always
/// unambiguous. For an OVH-hosted Exchange mailbox the usual choices are
/// [`Self::Tls`] on port `465` (`ssl0.ovh.net`) or [`Self::StartTls`] on port `587`
/// (`pro*.mail.ovh.net`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EmailSecurity {
    /// Implicit TLS from the first byte (SMTPS, typically port 465).
    Tls,
    /// Plain connection upgraded via the `STARTTLS` command (typically port 587).
    ///
    /// The default: STARTTLS on 587 is the most broadly compatible option and matches OVH's
    /// documented Exchange submission endpoint (`pro*.mail.ovh.net:587`).
    #[default]
    StartTls,
    /// No transport security (plaintext; only for a trusted local relay / tests).
    None,
}

/// Outgoing-email settings for transactional messages (welcome, password reset).
///
/// Two mutually exclusive ways to point at a relay:
/// 1. A single [`Self::url`] in lettre's `AsyncSmtpTransport::from_url` format
///    (e.g. `smtps://user:pass@ssl0.ovh.net:465`), which encodes host, port, TLS and
///    credentials at once and takes precedence when set.
/// 2. The explicit [`Self::host`]/[`Self::port`]/[`Self::username`]/[`Self::password`]/
///    [`Self::security`] fields, which read more naturally for an OVH Exchange mailbox.
///
/// The channel is only enabled when a relay (`url` or `host`) and a [`Self::from`] address
/// are both present; otherwise the app falls back to a no-op mailer that logs and drops.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct EmailConfig {
    /// Full lettre relay URL; takes precedence over the explicit fields below when set.
    #[serde(default)]
    pub url: Option<String>,
    /// SMTP host (OVH Exchange: `pro3.mail.ovh.net` for STARTTLS or `ssl0.ovh.net` for TLS).
    #[serde(default)]
    pub host: Option<String>,
    /// SMTP port. Defaults per [`Self::security`] when omitted (465 / 587 / 25).
    #[serde(default)]
    pub port: Option<u16>,
    /// Login username (for OVH Exchange this is the full mailbox address).
    #[serde(default)]
    pub username: Option<String>,
    /// Login password / app password.
    #[serde(default)]
    pub password: Option<String>,
    /// Transport security to use with the explicit host/port fields.
    #[serde(default)]
    pub security: EmailSecurity,
    /// Default `From` mailbox, e.g. `TankoVault <no-reply@example.com>`. Required to send.
    #[serde(default)]
    pub from: Option<String>,
    /// SMTP envelope sender (the `MAIL FROM` / `Return-Path`) used at the protocol level,
    /// which can differ from the visible [`Self::from`] header.
    ///
    /// Providers that enforce "send as" checks — notably **OVH-hosted Exchange** — reject a
    /// message whose envelope sender is not the authenticated mailbox (SMTP `550 5.7.60
    /// Client does not have permissions to send as this sender`). Leave this unset to default
    /// to [`Self::username`] (the authenticated login), which is what those providers require
    /// while still letting the `From:` header show a different address; set it explicitly only
    /// to override that reverse-path.
    #[serde(default)]
    pub envelope_from: Option<String>,
    /// Public base URL of the web app, used to build absolute links inside emails
    /// (e.g. the password-reset link). No trailing slash.
    #[serde(default = "EmailConfig::default_base_url")]
    pub base_url: String,
    /// Per-message send timeout, seconds.
    #[serde(default = "EmailConfig::default_timeout_secs")]
    pub timeout_secs: u64,
}

impl EmailConfig {
    fn default_base_url() -> String {
        "http://localhost:8080".to_owned()
    }

    fn default_timeout_secs() -> u64 {
        15
    }

    /// The effective port, applying the security-specific default when none is configured.
    #[must_use]
    pub fn effective_port(&self) -> u16 {
        self.port.unwrap_or(match self.security {
            EmailSecurity::Tls => 465,
            EmailSecurity::StartTls => 587,
            EmailSecurity::None => 25,
        })
    }

    /// The SMTP envelope sender (`MAIL FROM`), preferring an explicit [`Self::envelope_from`]
    /// and otherwise falling back to the authenticated [`Self::username`]. Returns `None` when
    /// neither is set, in which case the mailer uses the `From:` header address.
    #[must_use]
    pub fn effective_envelope_from(&self) -> Option<&str> {
        self.envelope_from
            .as_deref()
            .filter(|s| !s.is_empty())
            .or_else(|| self.username.as_deref().filter(|s| !s.is_empty()))
    }

    /// Whether enough is configured to actually send mail (a relay plus a `From` address).
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        let has_relay = self.url.as_deref().is_some_and(|u| !u.is_empty())
            || self.host.as_deref().is_some_and(|h| !h.is_empty());
        has_relay && self.from.as_deref().is_some_and(|f| !f.is_empty())
    }
}

/// Prometheus metrics facility.
///
/// Disabling this is a real off switch, not a filter: [`Self::enabled`] gates installation
/// of the process-wide recorder itself, so with metrics off no counter/histogram storage is
/// allocated and `metrics::counter!` calls compile down to a no-op dispatch against the
/// default (dropping) recorder.
#[derive(Debug, Clone, Deserialize)]
pub struct MetricsConfig {
    /// Install the Prometheus recorder and serve the scrape endpoint. When `false` the
    /// scrape route answers `404` and no measurements are retained.
    #[serde(default = "crate::default_true")]
    pub enabled: bool,
    /// Path the scrape endpoint is mounted at.
    #[serde(default = "MetricsConfig::default_route")]
    pub route: String,
    /// Address the Prometheus scrape endpoint binds to on its **own** listener, isolating
    /// it from the service's public HTTP port. When `Some`, the scrape route is removed
    /// from the main app and served only here (defaults to `0.0.0.0:9090`), so metrics can
    /// be kept on an internal-only interface and never share the request-facing port. When
    /// `None` the scrape stays merged onto the service's primary port (the historical
    /// behaviour).
    #[serde(default = "MetricsConfig::default_listen")]
    pub listen: Option<String>,
    /// Also record per-request HTTP metrics (`http_requests_total`,
    /// `http_request_duration_seconds`) from the middleware stack. Separate from
    /// [`Self::enabled`] because the request histogram is the expensive part: a service can
    /// keep cheap domain counters while dropping per-route cardinality.
    #[serde(default = "crate::default_true")]
    pub http_requests: bool,
}

impl MetricsConfig {
    fn default_route() -> String {
        "/metrics".to_owned()
    }

    // The `Option` is not redundant: this is the `#[serde(default = ..)]` provider for an
    // `Option<String>` field, so its return type must match the field's. Unwrapping it as
    // clippy suggests would stop it compiling as a serde default.
    #[allow(
        clippy::unnecessary_wraps,
        reason = "must match the Option<String> field it defaults"
    )]
    fn default_listen() -> Option<String> {
        Some("0.0.0.0:9090".to_owned())
    }
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            route: Self::default_route(),
            listen: Self::default_listen(),
            http_requests: true,
        }
    }
}

/// Runtime feature flags (`tankovault_domain::Feature`).
///
/// Only the *plumbing* is configured here — which features are on is an operator decision made
/// from the control plane at runtime and stored in `feature_flag_overrides`, not a deployment
/// setting. Putting the flag values in config would defeat the point: the whole reason flags
/// exist alongside the wiring-time toggles (metrics, audit, rate limiting) is that they change
/// without a redeploy.
#[derive(Debug, Clone, Deserialize)]
pub struct FeaturesConfig {
    /// Seconds between refreshes of a service's cached flag snapshot.
    ///
    /// This is the bound on how long a flag change takes to reach *other* replicas; the
    /// replica that served the change applies it immediately. Trading a few seconds of
    /// staleness for not hitting the database on every request is the right trade for a
    /// deployment-wide switch — but the window has to be short enough that an operator
    /// switching something off during an incident does not sit and wonder.
    #[serde(default = "FeaturesConfig::default_refresh_secs")]
    pub refresh_secs: u64,
}

impl FeaturesConfig {
    fn default_refresh_secs() -> u64 {
        15
    }

    /// The refresh interval, clamped to at least a second so a misconfigured `0` cannot turn
    /// the refresh loop into a busy spin against the database.
    #[must_use]
    pub fn refresh_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.refresh_secs.max(1))
    }
}

impl Default for FeaturesConfig {
    fn default() -> Self {
        Self {
            refresh_secs: Self::default_refresh_secs(),
        }
    }
}

/// Append-only audit trail for privileged and privacy-relevant actions (design §16).
#[derive(Debug, Clone, Deserialize)]
pub struct AuditConfig {
    /// Write audit records. When `false` a no-op sink is installed and call sites stay
    /// unchanged — auditing is a wiring decision, never an `if` in a handler.
    #[serde(default = "crate::default_true")]
    pub enabled: bool,
    /// Record the client IP alongside each event. Off by default: an IP is personal data
    /// under GDPR Art. 4(1), so retaining it is an explicit operator decision.
    #[serde(default)]
    pub record_ip: bool,
    /// Record the client `User-Agent` alongside each event.
    #[serde(default)]
    pub record_user_agent: bool,
    /// Days to retain audit records before the retention sweep deletes them. `0` disables
    /// the sweep and keeps records forever, which is rarely what a GDPR-scoped deployment
    /// wants (storage limitation, Art. 5(1)(e)).
    #[serde(default = "AuditConfig::default_retention_days")]
    pub retention_days: u32,
    /// Hours between retention sweeps. Ignored when [`Self::retention_days`] is `0`.
    #[serde(default = "AuditConfig::default_sweep_interval_hours")]
    pub sweep_interval_hours: u64,
}

impl AuditConfig {
    fn default_retention_days() -> u32 {
        365
    }
    fn default_sweep_interval_hours() -> u64 {
        24
    }

    /// Whether the background retention sweep should run.
    #[must_use]
    pub fn retention_enabled(&self) -> bool {
        self.enabled && self.retention_days > 0
    }
}

impl Default for AuditConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            record_ip: false,
            record_user_agent: false,
            retention_days: Self::default_retention_days(),
            sweep_interval_hours: Self::default_sweep_interval_hours(),
        }
    }
}

/// Where rate-limit counters live.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RateLimitBackend {
    /// Process-local counters. Correct for a single replica and for tests; with `N`
    /// replicas behind a load balancer the effective limit is `N` times the configured one.
    #[default]
    Memory,
    /// Shared counters in Redis, so the limit holds across every replica. Requires a
    /// `redis` block; the limiter fails **open** (allows the request) if Redis is
    /// unreachable, since a counter-store outage must not take the edge down.
    Redis,
}

/// A single token-bucket policy: sustained refill rate plus bucket depth.
///
/// The two numbers control different things and are deliberately independent:
/// [`Self::per_minute`] is how fast the bucket refills, [`Self::burst`] is how deep it is.
/// A burst *below* the sustained rate is the normal case, not a misconfiguration — the
/// default global policy allows 300 requests/minute but at most 60 back-to-back, which
/// absorbs a page load without letting a client spend a whole minute's budget instantly.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct RateLimitPolicy {
    /// Sustained requests allowed per minute per client key; the bucket's refill rate.
    pub per_minute: u32,
    /// Bucket depth: the most requests a client may spend back-to-back.
    pub burst: u32,
}

impl RateLimitPolicy {
    /// Construct a policy directly (used for defaults and in tests).
    #[must_use]
    pub const fn new(per_minute: u32, burst: u32) -> Self {
        Self { per_minute, burst }
    }

    /// Bucket capacity: the most requests a client can make in one instant.
    ///
    /// Clamped to at least 1 — a zero-depth bucket would reject every request forever,
    /// which is never what a misconfigured `0` is meant to express.
    #[must_use]
    pub const fn capacity(&self) -> u32 {
        if self.burst == 0 { 1 } else { self.burst }
    }
}

/// Inbound request rate limiting for a service's HTTP edge.
///
/// Distinct from the *outbound* crawl politeness in `tankovault-fetch`, which paces requests
/// this system makes to third-party providers.
#[derive(Debug, Clone, Deserialize)]
pub struct RateLimitConfig {
    /// Enforce limits. When `false` the layer is not mounted at all (no per-request cost).
    #[serde(default = "crate::default_true")]
    pub enabled: bool,
    /// Counter store. See [`RateLimitBackend`].
    #[serde(default)]
    pub backend: RateLimitBackend,
    /// Applies to any route without a stricter class below.
    #[serde(default = "RateLimitConfig::default_global")]
    pub global: RateLimitPolicy,
    /// Credential-handling routes (login, register, password reset, token refresh). Tight
    /// by design — this is the online-guessing control, so it is deliberately far below
    /// [`Self::global`].
    #[serde(default = "RateLimitConfig::default_auth")]
    pub auth: RateLimitPolicy,
    /// Routes that are cheap to call and expensive to serve (data export, scan triggers,
    /// sync push/pull).
    #[serde(default = "RateLimitConfig::default_expensive")]
    pub expensive: RateLimitPolicy,
    /// Trust `X-Forwarded-For` / `X-Real-IP` when deriving the client key.
    ///
    /// **Only enable behind a reverse proxy that overwrites these headers.** With this on
    /// and no such proxy, any client can forge a fresh identity per request and bypass the
    /// limiter entirely — hence the safe default of `false`.
    #[serde(default)]
    pub trust_forwarded_for: bool,
}

impl RateLimitConfig {
    fn default_global() -> RateLimitPolicy {
        RateLimitPolicy::new(300, 60)
    }
    fn default_auth() -> RateLimitPolicy {
        RateLimitPolicy::new(10, 5)
    }
    fn default_expensive() -> RateLimitPolicy {
        // Cheap-to-ask, costly-to-serve routes. Kept well below `global`, but the previous
        // `6/min, burst 2` was tight enough that an operator triggering a couple of scans
        // back-to-back — or an account page firing its export alongside a sync — tripped it.
        // A shallow double still throttles abuse while leaving room for legitimate bursts.
        RateLimitPolicy::new(30, 10)
    }
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            backend: RateLimitBackend::default(),
            global: Self::default_global(),
            auth: Self::default_auth(),
            expensive: Self::default_expensive(),
            trust_forwarded_for: false,
        }
    }
}

/// Cross-origin resource sharing.
///
/// The default is an **empty allowlist**, which rejects every cross-origin request. The
/// reference deployment serves the frontend and the API from one origin (the `frontend`
/// server proxies `/v1/*` to the API), so no CORS hop exists; a split-origin deployment must
/// name its origins explicitly.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CorsConfig {
    /// Exact origins allowed, e.g. `https://app.example.com`. Empty disables CORS entirely.
    #[serde(default)]
    pub allowed_origins: Vec<String>,
    /// Allow credentialed cross-origin requests (cookies, `Authorization`). Cannot be
    /// combined with a wildcard origin, which is one more reason the allowlist is explicit.
    #[serde(default)]
    pub allow_credentials: bool,
    /// `Access-Control-Max-Age`, seconds.
    #[serde(default = "CorsConfig::default_max_age_secs")]
    pub max_age_secs: u64,
}

impl CorsConfig {
    fn default_max_age_secs() -> u64 {
        3600
    }

    /// Whether any cross-origin request should be permitted.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        !self.allowed_origins.is_empty()
    }
}

/// Edge hardening applied by the shared middleware stack.
///
/// `struct_excessive_bools` is allowed deliberately. The lint exists to catch boolean
/// *parameters* that should have been an enum; these are independent operator toggles that
/// map one-to-one onto `TANKOVAULT_SECURITY__*` environment variables. Collapsing them into
/// an enum or a bitflag would make the config surface harder to write, not easier.
#[derive(Debug, Clone, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct SecurityConfig {
    /// Cross-origin policy. See [`CorsConfig`].
    #[serde(default)]
    pub cors: CorsConfig,
    /// Reject request bodies larger than this, in bytes, before they are buffered.
    #[serde(default = "SecurityConfig::default_max_body_bytes")]
    pub max_body_bytes: usize,
    /// Abort a request that has not produced a response within this many seconds.
    #[serde(default = "SecurityConfig::default_request_timeout_secs")]
    pub request_timeout_secs: u64,
    /// Emit `Strict-Transport-Security`. Only meaningful when the edge is reached over
    /// TLS; sending it over plain HTTP is ignored by browsers but still misleading.
    #[serde(default)]
    pub hsts: bool,
    /// `max-age` for the HSTS header, seconds (default: two years, the preload minimum).
    #[serde(default = "SecurityConfig::default_hsts_max_age_secs")]
    pub hsts_max_age_secs: u64,
    /// Send the baseline set of hardening headers (`X-Content-Type-Options`,
    /// `X-Frame-Options`, `Referrer-Policy`, `Cross-Origin-Resource-Policy`).
    #[serde(default = "crate::default_true")]
    pub security_headers: bool,
    /// Accept and echo an inbound `X-Request-Id` instead of always minting a fresh one.
    /// Requires a trusted proxy for the same reason as
    /// [`RateLimitConfig::trust_forwarded_for`] — a client-supplied id can otherwise be
    /// used to collide or poison log correlation.
    #[serde(default)]
    pub trust_request_id: bool,
    /// Serve the browsable API documentation (`/scalar`) and the `OpenAPI` document.
    ///
    /// **Defaults to off in the production profile and on everywhere else**, which is why
    /// this default is a function that reads the environment rather than a literal: the
    /// useful behaviour differs between the two, and requiring an operator to remember a
    /// third variable to switch it off is how it stays on.
    ///
    /// Unauthenticated, it hands an attacker the complete admin surface — every
    /// `/v1/admin/*` path, the permission vocabulary and exact request bodies — without a
    /// single failed probe. That is reconnaissance rather than compromise, but it removes
    /// the discovery cost of every other weakness.
    #[serde(default = "SecurityConfig::default_expose_api_docs")]
    pub expose_api_docs: bool,
}

impl SecurityConfig {
    fn default_max_body_bytes() -> usize {
        1024 * 1024 // 1 MiB: the largest legitimate body here is a provider config blob.
    }
    fn default_request_timeout_secs() -> u64 {
        30
    }
    fn default_hsts_max_age_secs() -> u64 {
        63_072_000
    }
    fn default_expose_api_docs() -> bool {
        !is_production()
    }
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            cors: CorsConfig::default(),
            max_body_bytes: Self::default_max_body_bytes(),
            request_timeout_secs: Self::default_request_timeout_secs(),
            hsts: false,
            hsts_max_age_secs: Self::default_hsts_max_age_secs(),
            security_headers: true,
            trust_request_id: false,
            expose_api_docs: Self::default_expose_api_docs(),
        }
    }
}

/// Shared `serde` default for fields that are on unless explicitly disabled.
///
/// Public so service-local config structs can use the same spelling. A security default that
/// each service re-derives is a security default that one of them will get wrong — which is
/// exactly what happened to `cookie_secure`, a `#[serde(default)]` `bool` that therefore
/// defaulted to *off*.
#[must_use]
pub fn default_true() -> bool {
    true
}

/// Authentication for service-to-service calls on the internal network.
///
/// `sync`, `control-plane`, `render` and `challenge-solver` expose privileged operations —
/// reading any user's sync state, triggering scans, fetching an arbitrary URL — and are
/// reachable by service name from anywhere on the compose/cluster network. Network
/// placement alone is one misconfiguration away from exposing them, so the calls carry a
/// shared secret in `X-Internal-Token`.
///
/// The token is deliberately **required** in the production profile: a service that starts
/// happily without one silently downgrades to the unauthenticated behaviour this exists to
/// remove. See [`InternalAuthConfig::resolve`].
#[derive(Debug, Clone, Default, Deserialize)]
pub struct InternalAuthConfig {
    /// Shared secret presented by internal callers. Generate with `openssl rand -hex 32`.
    #[serde(default)]
    pub token: Option<String>,
}

/// The shortest token accepted. 32 bytes of hex is the documented recipe; anything
/// materially shorter is guessable at internal-network request rates.
pub const MIN_INTERNAL_TOKEN_LEN: usize = 32;

impl InternalAuthConfig {
    /// The configured token, or an error explaining why the service must not start.
    ///
    /// Outside the production profile a missing token is allowed and reported as `None`, so
    /// `docker compose up` and the test harness stay frictionless. A token that is *present*
    /// is always length-checked, in every profile — a weak-secret check a deployment can skip
    /// by forgetting one variable is not a check.
    ///
    /// # Errors
    /// When the token is absent in the production profile, or shorter than
    /// [`MIN_INTERNAL_TOKEN_LEN`].
    pub fn resolve(&self, production: bool) -> Result<Option<String>, ConfigError> {
        match self
            .token
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
        {
            None if production => Err(ConfigError::Invalid(
                "internal.token is required when TANKOVAULT_PROFILE=production; generate one \
                 with `openssl rand -hex 32` and set TANKOVAULT_INTERNAL__TOKEN on every \
                 service that talks to another"
                    .to_owned(),
            )),
            None => Ok(None),
            Some(t) if t.len() < MIN_INTERNAL_TOKEN_LEN => Err(ConfigError::Invalid(format!(
                "internal.token must be at least {MIN_INTERNAL_TOKEN_LEN} characters, got {}",
                t.len()
            ))),
            Some(t) => Ok(Some(t.to_owned())),
        }
    }
}

/// Whether the process is running under the production profile.
///
/// A single spelling of the check, so the guards that key off it cannot drift apart.
#[must_use]
pub fn is_production() -> bool {
    std::env::var("TANKOVAULT_PROFILE").is_ok_and(|p| p.eq_ignore_ascii_case("production"))
}

/// The canonical source key for data pulled from `AniList` (its public GraphQL metadata or a
/// linked user's list). Kept as a shared constant so every service agrees on the spelling.
pub const SOURCE_ANILIST: &str = "anilist";
/// The canonical source key for data scraped by the local provider adapters.
pub const SOURCE_ADAPTER: &str = "adapter";

/// Which upstream source has the final say on each piece of series metadata.
///
/// Every field holds an ordered preference list, highest priority first; the first source
/// in the list that actually supplies a (non-blank) value wins. Unknown fields fall back to
/// [`Self::default`]. The out-of-the-box order is **`AniList` before the adapters**, matching
/// the design intent that a linked `AniList` sync is authoritative over scraped data.
#[derive(Debug, Clone, Deserialize)]
pub struct MetadataPriorityConfig {
    /// Priority order for the long-form series description.
    #[serde(default = "MetadataPriorityConfig::default_order")]
    pub description: Vec<String>,
    /// Priority order for the canonical/display title.
    #[serde(default = "MetadataPriorityConfig::default_order")]
    pub title: Vec<String>,
    /// Priority order for the cover image URL.
    #[serde(default = "MetadataPriorityConfig::default_order")]
    pub cover: Vec<String>,
    /// Fallback order for any field not listed above.
    #[serde(default = "MetadataPriorityConfig::default_order")]
    pub default: Vec<String>,
}

impl MetadataPriorityConfig {
    /// The default preference: `AniList` wins, then the scraping adapters.
    fn default_order() -> Vec<String> {
        vec![SOURCE_ANILIST.to_owned(), SOURCE_ADAPTER.to_owned()]
    }

    /// The configured priority order for a named field, falling back to `default` when the
    /// field is unknown or its list was explicitly emptied.
    #[must_use]
    pub fn order_for(&self, field: &str) -> &[String] {
        let list = match field {
            "description" => &self.description,
            "title" => &self.title,
            "cover" => &self.cover,
            _ => &self.default,
        };
        if list.is_empty() { &self.default } else { list }
    }

    /// Pick the winning value for `field`.
    ///
    /// `candidates` maps a source key (e.g. [`SOURCE_ANILIST`], [`SOURCE_ADAPTER`]) to its
    /// optional value. The first source in the field's priority order that supplies a
    /// non-blank value wins. If no prioritised source matches, any present candidate is used
    /// as a last resort (so a value is never dropped just because its source was unlisted).
    #[must_use]
    pub fn resolve(&self, field: &str, candidates: &[(&str, Option<String>)]) -> Option<String> {
        for source in self.order_for(field) {
            for (src, value) in candidates {
                if *src == source.as_str() {
                    if let Some(v) = value {
                        if !v.trim().is_empty() {
                            return Some(v.clone());
                        }
                    }
                }
            }
        }
        candidates
            .iter()
            .find_map(|(_, v)| v.clone().filter(|s| !s.trim().is_empty()))
    }
}

impl Default for MetadataPriorityConfig {
    fn default() -> Self {
        Self {
            description: Self::default_order(),
            title: Self::default_order(),
            cover: Self::default_order(),
            default: Self::default_order(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Deserialize)]
    struct Sample {
        database: DatabaseConfig,
        #[serde(default)]
        http: HttpConfig,
    }

    #[test]
    // `figment::Jail::expect_with` fixes the closure's error type to the large `figment::Error`.
    #[allow(clippy::result_large_err)]
    fn env_overrides_and_defaults_apply() {
        figment::Jail::expect_with(|jail| {
            jail.set_env(
                "TANKOVAULT_DATABASE__URL",
                "postgres://localhost/tankovault",
            );
            jail.set_env("TANKOVAULT_DATABASE__MAX_CONNECTIONS", "32");
            let cfg: Sample = load().map_err(|e| e.to_string()).unwrap();
            assert_eq!(cfg.database.url, "postgres://localhost/tankovault");
            assert_eq!(cfg.database.max_connections, 32);
            // Untouched nested default still applies.
            assert_eq!(cfg.database.acquire_timeout_secs, 10);
            assert_eq!(cfg.http.bind_addr, "0.0.0.0:8080");
            Ok(())
        });
    }

    #[test]
    fn email_disabled_without_relay_or_from() {
        // Nothing configured → disabled.
        assert!(!EmailConfig::default().is_enabled());
        // Relay but no `From` → still disabled.
        let cfg = EmailConfig {
            host: Some("pro3.mail.ovh.net".to_owned()),
            ..EmailConfig::default()
        };
        assert!(!cfg.is_enabled());
    }

    #[test]
    fn email_enabled_with_host_and_from() {
        let cfg = EmailConfig {
            host: Some("pro3.mail.ovh.net".to_owned()),
            from: Some("no-reply@example.com".to_owned()),
            ..EmailConfig::default()
        };
        assert!(cfg.is_enabled());
    }

    #[test]
    fn email_port_defaults_follow_security() {
        let starttls = EmailConfig::default();
        assert_eq!(starttls.security, EmailSecurity::StartTls);
        assert_eq!(starttls.effective_port(), 587);

        let tls = EmailConfig {
            security: EmailSecurity::Tls,
            ..EmailConfig::default()
        };
        assert_eq!(tls.effective_port(), 465);

        let explicit = EmailConfig {
            port: Some(2525),
            security: EmailSecurity::None,
            ..EmailConfig::default()
        };
        assert_eq!(explicit.effective_port(), 2525);
    }

    #[test]
    fn priority_defaults_prefer_anilist_over_adapter() {
        let cfg = MetadataPriorityConfig::default();
        let winner = cfg.resolve(
            "description",
            &[
                (SOURCE_ADAPTER, Some("scraped blurb".to_owned())),
                (SOURCE_ANILIST, Some("anilist blurb".to_owned())),
            ],
        );
        assert_eq!(winner.as_deref(), Some("anilist blurb"));
    }

    #[test]
    fn priority_falls_through_to_next_source_when_higher_is_blank() {
        let cfg = MetadataPriorityConfig::default();
        // AniList is highest priority but supplies nothing usable → the adapter wins.
        let winner = cfg.resolve(
            "description",
            &[
                (SOURCE_ANILIST, Some("   ".to_owned())),
                (SOURCE_ADAPTER, Some("scraped blurb".to_owned())),
            ],
        );
        assert_eq!(winner.as_deref(), Some("scraped blurb"));

        let winner_none = cfg.resolve(
            "description",
            &[
                (SOURCE_ANILIST, None),
                (SOURCE_ADAPTER, Some("only one".to_owned())),
            ],
        );
        assert_eq!(winner_none.as_deref(), Some("only one"));
    }

    #[test]
    fn priority_respects_a_configured_adapter_first_order() {
        let cfg = MetadataPriorityConfig {
            description: vec![SOURCE_ADAPTER.to_owned(), SOURCE_ANILIST.to_owned()],
            ..MetadataPriorityConfig::default()
        };
        let winner = cfg.resolve(
            "description",
            &[
                (SOURCE_ANILIST, Some("anilist blurb".to_owned())),
                (SOURCE_ADAPTER, Some("scraped blurb".to_owned())),
            ],
        );
        assert_eq!(winner.as_deref(), Some("scraped blurb"));
    }

    #[test]
    fn unknown_field_uses_default_order_and_last_resort() {
        let cfg = MetadataPriorityConfig::default();
        // Unknown field → falls back to `default` order (anilist first).
        assert_eq!(cfg.order_for("banner"), cfg.default.as_slice());
        // A value from an unlisted source is still used rather than dropped.
        let winner = cfg.resolve("description", &[("mangaupdates", Some("x".to_owned()))]);
        assert_eq!(winner.as_deref(), Some("x"));
        // Nothing anywhere → nothing.
        assert_eq!(cfg.resolve("description", &[(SOURCE_ANILIST, None)]), None);
    }

    #[test]
    fn empty_configured_list_falls_back_to_default() {
        let cfg = MetadataPriorityConfig {
            description: Vec::new(),
            ..MetadataPriorityConfig::default()
        };
        assert_eq!(cfg.order_for("description"), cfg.default.as_slice());
    }
}
