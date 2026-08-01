//! Reads TOML and environment variables into a service's config struct; also the shared
//! `serde`-default helpers used across this crate.

use figment::{
    Figment,
    providers::{Env, Format, Toml},
};
use serde::de::DeserializeOwned;

use crate::error::ConfigError;

/// Load a typed config for a service: a TOML file (`$TANKOVAULT_CONFIG`, default
/// `config.toml`) merged under `TANKOVAULT_`-prefixed, `__`-nested environment variables.
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
/// One spelling so no service re-derives it wrong (`cookie_secure` once defaulted to *off*).
#[must_use]
pub fn default_true() -> bool {
    true
}

/// Whether the process runs under the production profile; one spelling so guards cannot
/// drift apart.
#[must_use]
pub fn is_production() -> bool {
    std::env::var("TANKOVAULT_PROFILE").is_ok_and(|p| p.eq_ignore_ascii_case("production"))
}

#[cfg(test)]
mod tests {
    use crate::{DatabaseConfig, MetricsConfig, load};
    use secrecy::ExposeSecret as _;
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
            // `SecretString` has no `PartialEq`; comparing requires `expose_secret()`.
            assert_eq!(
                cfg.database.url.expose_secret(),
                "postgres://localhost/tankovault"
            );
            assert_eq!(cfg.database.max_connections, 32);
            // Untouched nested default still applies.
            assert_eq!(cfg.database.acquire_timeout_secs, 10);
            // A block untouched by the environment still materialises with its own defaults.
            assert_eq!(cfg.metrics.route, "/metrics");
            Ok(())
        });
    }
}
