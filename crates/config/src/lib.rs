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
    /// OTLP collector endpoint; when `None`, tracing exports to stdout only.
    #[serde(default)]
    pub otlp_endpoint: Option<String>,
    /// `RUST_LOG`-style filter (e.g. `info,tankovault=debug`).
    #[serde(default = "TelemetryConfig::default_log_filter")]
    pub log_filter: String,
    /// Emit structured JSON logs (production) vs. pretty logs (dev).
    #[serde(default)]
    pub json_logs: bool,
    /// Address the Prometheus scrape endpoint binds to, if enabled.
    #[serde(default)]
    pub metrics_addr: Option<String>,
}

impl TelemetryConfig {
    fn default_log_filter() -> String {
        "info".to_owned()
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
            jail.set_env("TANKOVAULT_DATABASE__URL", "postgres://localhost/tankovault");
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
}
