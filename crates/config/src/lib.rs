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
            &[(SOURCE_ANILIST, None), (SOURCE_ADAPTER, Some("only one".to_owned()))],
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
