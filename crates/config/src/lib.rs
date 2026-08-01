//! Layered, typed configuration: struct defaults, then an optional TOML file, then
//! `TANKOVAULT_`-prefixed environment variables (highest precedence). Call [`load`].

mod audit;
mod cors;
mod database;
mod email;
mod error;
mod features;
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
pub use internal_auth::{InternalAuthConfig, MIN_INTERNAL_TOKEN_LEN};
pub use loader::{default_true, is_production, load};
pub use matching::MatchingConfig;
pub use messaging::{NatsConfig, RedisConfig};
pub use metrics::MetricsConfig;
pub use ratelimit::{RateLimitBackend, RateLimitConfig, RateLimitPolicy};
pub use security::SecurityConfig;
pub use telemetry::TelemetryConfig;
