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
//!
//! # Layout (ARCH-8)
//!
//! One module per aggregate, all re-exported here so `tankovault_config::DatabaseConfig`
//! keeps working — the split is internal and breaks no caller. It replaces a single flat file
//! holding fifteen unrelated aggregates, which every service in the workspace compiled in full
//! to use one of them.
//!
//! What is deliberately **not** here: `MetadataPriorityConfig` and its `SOURCE_*` string keys
//! moved to `tankovault_domain::metadata_priority`. "`AniList`'s description beats the
//! adapter's" is a statement about the catalogue, not about configuration layering, and this
//! crate does not depend on `tankovault-domain` — so the policy could not name a domain type
//! and had to be stringly typed. It is now a closed `MetadataSource` enum.

mod audit;
mod cors;
mod database;
mod email;
mod error;
mod features;
mod http;
mod internal_auth;
mod loader;
mod matching;
mod messaging;
mod metrics;
mod ratelimit;
mod security;
mod telemetry;

pub use audit::AuditConfig;
pub use cors::CorsConfig;
pub use database::DatabaseConfig;
pub use email::{EmailConfig, EmailSecurity};
pub use error::ConfigError;
pub use features::FeaturesConfig;
pub use http::HttpConfig;
pub use internal_auth::{InternalAuthConfig, MIN_INTERNAL_TOKEN_LEN};
pub use loader::{default_true, is_production, load};
pub use matching::MatchingConfig;
pub use messaging::{NatsConfig, RedisConfig};
pub use metrics::MetricsConfig;
pub use ratelimit::{RateLimitBackend, RateLimitConfig, RateLimitPolicy};
pub use security::SecurityConfig;
pub use telemetry::TelemetryConfig;
