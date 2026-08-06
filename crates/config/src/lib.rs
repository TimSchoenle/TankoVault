//! Layered, typed configuration. Lowest precedence first: struct defaults, TOML at
//! `$TANKOVAULT_CONFIG` (a file, or every `*.toml` in it when it names a directory),
//! `TANKOVAULT_`-prefixed `__`-nested environment variables, `$TANKOVAULT_SECRETS_DIR`, and
//! `TANKOVAULT_<KEY>_FILE` indirection.
//!
//! Call [`load`], or [`load_watched`] when the process should be able to pick the config up
//! again after a mounted file changes.
//!
//! The last three layers are mutually exclusive per key: a key supplied by two of them is
//! refused at boot rather than resolved by precedence; `src/secrets.rs` says why.

mod audit;
mod chapter_outliers;
mod cors;
mod database;
mod email;
mod error;
mod features;
mod internal_auth;
mod legal;
mod loader;
mod matching;
mod messaging;
mod metadata;
mod metrics;
mod ratelimit;
mod secrets;
mod security;
mod telemetry;

pub use audit::AuditConfig;
pub use chapter_outliers::ChapterOutlierConfig;
pub use cors::CorsConfig;
pub use database::DatabaseConfig;
pub use email::{EmailConfig, EmailSecurity};
pub use error::ConfigError;
pub use features::FeaturesConfig;
pub use internal_auth::{InternalAuthConfig, MIN_INTERNAL_TOKEN_LEN};
pub use legal::{LegalConfig, LegalDocument};
pub use loader::{Loaded, Sources, default_true, is_production, load, load_watched};
pub use matching::MatchingConfig;
pub use messaging::{NatsConfig, RedisConfig};
pub use metadata::{MetadataPriorityConfig, TagIntakeConfig};
pub use metrics::MetricsConfig;
pub use ratelimit::{RateLimitBackend, RateLimitConfig, RateLimitPolicy};
pub use security::SecurityConfig;
pub use telemetry::TelemetryConfig;
