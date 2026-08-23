//! The typed configuration surface: every block a service deserialises, plus the `TankoVault`
//! dialect of the layered loader.
//!
//! The layering is [`terrace_config`]'s. Lowest precedence first: struct defaults, TOML at
//! `$TANKOVAULT_CONFIG` (a file, or every `*.toml` in it when it names a directory),
//! `TANKOVAULT_`-prefixed `__`-nested environment variables, `$TANKOVAULT_SECRETS_DIR`, and
//! `TANKOVAULT_<KEY>_FILE` indirection. The last three are mutually exclusive per key: a key
//! supplied by two of them is refused at boot rather than resolved by precedence, because a
//! stale environment variable shadowing a rotated mounted secret keeps the service running on
//! the old credential.
//!
//! Call [`load`], or [`load_watched`] when the process should be able to pick the config up
//! again after a mounted file changes.

mod audit;
mod branding;
mod chapter_outliers;
mod client;
mod cors;
mod database;
mod email;
mod features;
mod internal_auth;
mod legal;
mod loader;
mod matching;
mod messaging;
mod metadata;
mod metrics;
mod ratelimit;
mod security;
mod sentry;
mod telemetry;

pub use audit::AuditConfig;
pub use branding::{BrandingConfig, CopyrightConfig, LicenceConfig, WordmarkConfig};
pub use chapter_outliers::ChapterOutlierConfig;
pub use client::ClientConfig;
pub use cors::CorsConfig;
pub use database::DatabaseConfig;
pub use email::{EmailConfig, EmailSecurity};
pub use features::FeaturesConfig;
pub use internal_auth::{
    CallerConfig, IdentityMode, InternalAuthConfig, InternalTlsConfig, MIN_INTERNAL_TOKEN_LEN,
    PeerConfig, ResolvedCaller, ResolvedInternalAuth, ResolvedPeer, ResolvedTls,
};
pub use legal::{LegalConfig, LegalDocument};
pub use loader::{
    ConfigError, Explanation, Layer, Loaded, Sources, default_true, explain, is_production, load,
    load_watched, terrace,
};
pub use matching::MatchingConfig;
pub use messaging::{NatsConfig, RedisConfig};
pub use metadata::{MetadataPriorityConfig, TagIntakeConfig};
pub use metrics::MetricsConfig;
pub use ratelimit::{RateLimitBackend, RateLimitConfig, RateLimitPolicy};
pub use security::SecurityConfig;
pub use sentry::{SentryConfig, SentryLevel};
pub use telemetry::TelemetryConfig;
