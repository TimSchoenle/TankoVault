//! The layering machinery itself: reading TOML and environment variables into a service's own
//! config struct, plus the two helpers every aggregate in this crate shares.

use figment::{
    Figment,
    providers::{Env, Format, Toml},
};
use serde::de::DeserializeOwned;

use crate::error::ConfigError;

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

/// Whether the process is running under the production profile.
///
/// A single spelling of the check, so the guards that key off it cannot drift apart.
#[must_use]
pub fn is_production() -> bool {
    std::env::var("TANKOVAULT_PROFILE").is_ok_and(|p| p.eq_ignore_ascii_case("production"))
}

#[cfg(test)]
mod tests {
    use crate::{DatabaseConfig, MetricsConfig, load};
    use serde::Deserialize;

    #[derive(Debug, Deserialize)]
    struct Sample {
        database: DatabaseConfig,
        #[serde(default)]
        metrics: MetricsConfig,
    }

    #[test]
    // `figment::Jail::expect_with` fixes the closure's error type to the large `figment::Error`.
    #[expect(
        clippy::result_large_err,
        reason = "figment::Jail::expect_with fixes the closure's error type"
    )]
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
            // A block nothing in the environment mentions at all still materialises with its
            // own defaults, rather than being absent.
            assert_eq!(cfg.metrics.route, "/metrics");
            Ok(())
        });
    }
}
