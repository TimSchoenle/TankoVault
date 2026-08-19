//! What the `api` binary reads from its configuration.
//!
//! Public, and in the library rather than beside `main`, because it is the root
//! `config-contract` describes for this image: the contract has to be generated from the very
//! type the binary deserialises, or it is a claim about something else.

use secrecy::SecretString;
use terrace_config::schema::Describe;

#[derive(Debug, serde::Deserialize, Describe)]
pub struct Config {
    #[config(nested)]
    pub database: tankovault_config::DatabaseConfig,
    #[config(nested)]
    pub telemetry: tankovault_config::TelemetryConfig,
    #[config(nested)]
    pub auth: AuthConfig,
    /// The public listener. Everything a reader's browser and the native client reach is
    /// served from here, behind whatever terminates TLS.
    #[serde(default = "default_bind")]
    pub bind_addr: String,
    #[serde(default = "default_control_plane")]
    pub control_plane_url: String,
    #[serde(default = "default_sync")]
    pub sync_url: String,
    #[serde(default = "default_worker")]
    pub worker_url: String,
    /// NATS for the live SSE relay; absent or unreachable only degrades `/v1/me/stream`.
    #[serde(default)]
    #[config(nested)]
    pub nats: Option<tankovault_config::NatsConfig>,
    /// Redis for cross-replica rate-limit counters; falls back to per-replica in-memory without it.
    #[serde(default)]
    #[config(nested)]
    pub redis: Option<tankovault_config::RedisConfig>,
    /// Transactional email (registration, password reset); unconfigured falls back to a no-op mailer.
    #[serde(default)]
    #[config(nested)]
    pub email: tankovault_config::EmailConfig,
    /// Edge hardening: CORS allowlist, body cap, request timeout, security headers.
    #[serde(default)]
    #[config(nested)]
    pub security: tankovault_config::SecurityConfig,
    /// Inbound rate limiting. Togglable; see `tankovault_config::RateLimitConfig`.
    #[serde(default)]
    #[config(nested)]
    pub rate_limit: tankovault_config::RateLimitConfig,
    /// Prometheus metrics. Togglable; disabling installs no recorder at all.
    #[serde(default)]
    #[config(nested)]
    pub metrics: tankovault_config::MetricsConfig,
    /// Audit trail. Togglable; disabling installs a no-op sink.
    #[serde(default)]
    #[config(nested)]
    pub audit: tankovault_config::AuditConfig,
    /// Runtime feature flags; only refresh cadence lives here — on/off is an operator
    /// decision made in the control plane at runtime.
    #[serde(default)]
    #[config(nested)]
    pub features: tankovault_config::FeaturesConfig,
    /// Shared secret presented to `sync`, `control-plane` and `challenge-solver`; must match
    /// across every service in the internal tier.
    #[serde(default)]
    #[config(nested)]
    pub internal: tankovault_config::InternalAuthConfig,
    /// Operator-published legal documents (Terms, Data Policy, Imprint, ...). An absent
    /// section publishes nothing, which is a valid deployment.
    #[serde(default)]
    #[config(nested)]
    pub legal: tankovault_config::LegalConfig,
    /// What this deployment calls itself: name, wordmark, copyright, links. Served to the
    /// client at `/v1/branding` and stamped into email and the authenticator prompts.
    #[serde(default)]
    #[config(nested)]
    pub branding: tankovault_config::BrandingConfig,
    /// Which repository the native client updates from, and the client versions this
    /// deployment supports. Served at `/v1/client`; an absent section publishes the upstream
    /// channel with this build's own version as the ceiling.
    #[serde(default)]
    #[config(nested)]
    pub client: tankovault_config::ClientConfig,
    /// Metadata intake rules. The API writes no metadata; it reads this section for the adult
    /// classifier alone, and shares it with the worker so the genres the public tag facet
    /// withholds are exactly the ones that put a series behind the gate.
    #[serde(default)]
    #[config(nested)]
    pub metadata: tankovault_config::MetadataPriorityConfig,
}

#[derive(Debug, serde::Deserialize, Describe)]
pub struct AuthConfig {
    /// HS256 signing key for access tokens; wrapped in `SecretString` so a `tracing::debug!(?cfg)`
    /// on this Debug-deriving, nested struct can't publish the key that authenticates every session.
    #[config(secret)]
    pub jwt_secret: SecretString,
    /// Server-side password pepper: mixed into every argon2id hash so a leak alone can't be
    /// brute-forced offline. Empty (default) is un-peppered, for compatibility with old
    /// hashes; once configured it must stay stable or passwords stop verifying.
    #[serde(default)]
    #[config(secret)]
    pub password_pepper: SecretString,
    #[serde(default = "default_access_minutes")]
    pub access_ttl_minutes: i64,
    #[serde(default = "default_refresh_days")]
    pub refresh_ttl_days: i64,
    /// Mark the refresh cookie `Secure`.
    ///
    /// Defaults to true — a `bool`'s implicit default is `false`, which would silently send a
    /// 30-day refresh credential over plain HTTP.
    ///
    /// Also selects the cookie's name/path (`__Host-` vs unprefixed), since a `__Host-`
    /// cookie without `Secure` is refused by the browser. Flipping this forces one re-login.
    #[serde(default = "tankovault_config::default_true")]
    pub cookie_secure: bool,
    /// Public origin of the web app, for passkeys — `https://tanko.example.com`.
    ///
    /// Cannot be inferred from a request: `Host` is attacker-controlled, and trusting it
    /// would let anyone mint credentials under a domain of their choosing. Unset falls back
    /// to [`tankovault_config::EmailConfig::base_url`] and disables only passkeys.
    #[serde(default)]
    pub webauthn_origin: Option<String>,
    /// Relying-party id: the registrable domain credentials are bound to. Defaults to
    /// [`Self::webauthn_origin`]'s host. Set to a parent domain only if the app moves between
    /// subdomains and keys must survive the move.
    #[serde(default)]
    pub webauthn_rp_id: Option<String>,
    /// The name the authenticator shows in its prompt ("Save a passkey for …"); purely cosmetic.
    #[serde(default)]
    pub webauthn_rp_name: Option<String>,
    /// Base64 (standard alphabet) 32-byte key that seals TOTP secrets at rest.
    ///
    /// Deliberately *not* derived from [`Self::jwt_secret`]: rotating the signing key is a
    /// routine operation, and deriving from it would silently invalidate every enrolled
    /// authenticator app — the same class of failure as rotating `password_pepper` and
    /// stranding the seeded admin. Unset disables TOTP enrolment only; security keys and
    /// recovery codes are unaffected, since neither stores a symmetric secret.
    #[serde(default)]
    #[config(secret)]
    pub mfa_encryption_key: Option<SecretString>,
    /// The issuer name an authenticator app files the entry under. Defaults to
    /// [`Self::webauthn_rp_name`], then to the product name — one label for both prompts.
    #[serde(default)]
    pub totp_issuer: Option<String>,
    /// How long a step-up elevation survives **unused** before a sensitive action prompts again.
    /// Every elevated request slides it forward.
    #[serde(default = "default_step_up_minutes")]
    pub step_up_ttl_minutes: i64,
    /// The ceiling on that sliding: the longest an elevation is honoured after it was earned,
    /// however continuously it is used.
    #[serde(default = "default_step_up_max_minutes")]
    pub step_up_max_ttl_minutes: i64,
    /// How long a half-finished sign-in — password accepted, second factor still owed — may
    /// sit before the user has to start again.
    #[serde(default = "default_mfa_challenge_minutes")]
    pub mfa_challenge_ttl_minutes: i64,
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
