//! The `TankoVault` dialect of [`terrace_config`], and the `serde` default helpers this crate
//! shares.
//!
//! `terrace-config` owns the layering: the TOML fragments, the `TANKOVAULT_*` environment layer,
//! the secrets-directory provider, the `_FILE` indirection and the shadow-key rejection. This
//! module names the environment variables and says which of them a file may not supply.

use serde::de::DeserializeOwned;
use terrace_config::Terrace;

pub use terrace_config::explain::{Explanation, Layer};
pub use terrace_config::{Error as ConfigError, Loaded, Sources};

/// The prefix every configuration variable carries.
const PREFIX: &str = "TANKOVAULT_";

/// Returns the loader every service boots through.
///
/// Layers, lowest precedence first: struct defaults, TOML at `$TANKOVAULT_CONFIG` (a file, or
/// every `*.toml` in it if it names a directory), `TANKOVAULT_`-prefixed `__`-nested
/// environment variables, `$TANKOVAULT_SECRETS_DIR`, and `TANKOVAULT_<KEY>_FILE` indirection.
/// The last three are mutually exclusive per key: a key supplied by two of them is refused at
/// boot.
///
/// A reserved key is read straight from the environment, before or outside the layered config,
/// so a file may not supply one. A secrets directory or a `_FILE` variable naming one fails the
/// boot. `TANKOVAULT_CONFIG` and `TANKOVAULT_SECRETS_DIR` are reserved by `terrace-config`
/// itself, since both decide what the layers are.
#[must_use]
pub fn terrace() -> Terrace {
    Terrace::new(PREFIX)
        // Every name is a literal, including the two `Terrace::new(PREFIX)` derives: `xtask
        // config-docs` reads the documented variables from the literals in this tree.
        .config_var("TANKOVAULT_CONFIG")
        .secrets_dir_var("TANKOVAULT_SECRETS_DIR")
        // Read by `is_production`, which every production guard is spelled in terms of.
        .reserve("TANKOVAULT_PROFILE")
        // Read by `xtask reset`, which never builds a figment at all.
        .reserve("TANKOVAULT_CONFIRM_RESET")
}

/// Loads a service's typed config from every layer.
///
/// # Errors
/// [`ConfigError`] if `TANKOVAULT_PROFILE` names no known profile, a required value is
/// missing, a value fails to parse, a file-backed source cannot be read, or one key is supplied
/// by more than one of the last three layers.
pub fn load<T: DeserializeOwned>() -> Result<T, ConfigError> {
    Profile::from_env()?;
    terrace().load()
}

/// Loads a typed config and everything a reload needs to load it again.
///
/// # Errors
/// As [`load`].
pub fn load_watched<T: DeserializeOwned>() -> Result<Loaded<T>, ConfigError> {
    Profile::from_env()?;
    terrace().load_watched()
}

/// Reports which layer supplied each key, without loading a config.
///
/// Re-reads the layers when called, so it is as valid inside a reload as at boot. It carries
/// key paths and the layers they came from, and no configuration value, so it is safe to log.
///
/// # Errors
/// As [`load`], except that a key supplied by more than one layer is reported, not refused.
pub fn explain() -> Result<Explanation, ConfigError> {
    terrace().explain()
}

/// Returns `true`: the `serde` default for a field that is on unless disabled.
#[must_use]
pub fn default_true() -> bool {
    true
}

/// The deployment profile named by `TANKOVAULT_PROFILE`.
///
/// [`Profile::parse`] is the only parser, so every production guard reads the same value the
/// same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    /// Unset, `development` or `dev`.
    Development,
    /// `production` or `prod`.
    Production,
}

impl Profile {
    /// Parses a raw `TANKOVAULT_PROFILE` value, trimmed and case-insensitive.
    ///
    /// # Errors
    /// [`ConfigError::Invalid`] for any other value, so an unrecognised profile stops the boot
    /// instead of meaning development.
    pub fn parse(raw: Option<&str>) -> Result<Self, ConfigError> {
        let value = raw.map(str::trim).unwrap_or_default().to_ascii_lowercase();
        match value.as_str() {
            "" | "development" | "dev" => Ok(Self::Development),
            "production" | "prod" => Ok(Self::Production),
            _ => Err(ConfigError::Invalid(
                "TANKOVAULT_PROFILE must be `production`, `prod`, `development`, `dev` or unset"
                    .to_owned(),
            )),
        }
    }

    /// Returns the profile this process runs under.
    ///
    /// # Errors
    /// As [`Self::parse`], and for a value that is not valid Unicode.
    pub fn from_env() -> Result<Self, ConfigError> {
        match std::env::var("TANKOVAULT_PROFILE") {
            Ok(raw) => Self::parse(Some(&raw)),
            Err(std::env::VarError::NotPresent) => Ok(Self::Development),
            Err(std::env::VarError::NotUnicode(_)) => Err(ConfigError::Invalid(
                "TANKOVAULT_PROFILE is not valid Unicode".to_owned(),
            )),
        }
    }
}

/// Returns whether the process runs under the production profile.
///
/// Every production guard calls this. An unrecognised profile answers `true`, so a guard
/// consulted before [`load`] has refused the boot fails closed.
#[must_use]
pub fn is_production() -> bool {
    Profile::from_env().map_or(true, |p| p == Profile::Production)
}

#[cfg(test)]
mod profile_tests {
    use super::Profile;

    /// Every accepted spelling, in any letter case, parses to the same profile.
    #[test]
    fn every_spelling_of_a_profile_parses_the_same_everywhere() {
        for raw in ["production", "Production", "PRODUCTION", "prod", " prod "] {
            assert_eq!(
                Profile::parse(Some(raw)).unwrap(),
                Profile::Production,
                "{raw}"
            );
        }
        for raw in [None, Some(""), Some("development"), Some("Dev")] {
            assert_eq!(
                Profile::parse(raw).unwrap(),
                Profile::Development,
                "{raw:?}"
            );
        }
    }

    /// An unknown profile is refused, so it cannot disarm the production guards by meaning
    /// development.
    #[test]
    fn an_unknown_profile_is_refused() {
        assert!(Profile::parse(Some("staging")).is_err());
        assert!(Profile::parse(Some("production-eu")).is_err());
    }
}

#[cfg(test)]
mod tests {
    use crate::{DatabaseConfig, MetricsConfig, load};
    use secrecy::ExposeSecret as _;
    use serde::Deserialize;
    use terrace_config::testing::Harness;

    use super::Layer;

    #[derive(Debug, Deserialize)]
    struct Sample {
        database: DatabaseConfig,
        #[serde(default)]
        metrics: MetricsConfig,
    }

    /// A sandbox over the real dialect, so every name these tests arrange is the name
    /// [`super::terrace`] reads. A name spelled out by hand would keep passing after the prefix
    /// changed.
    fn harness() -> Harness {
        Harness::over(super::terrace())
    }

    /// The prefix, the `__` nesting, and the defaults around what the environment supplied.
    /// `terrace-config` tests the layering itself; these tests pin the names an operator sets.
    ///
    /// They call `load()`, not `jail.load()`, because the entry point every service boots
    /// through is under test too.
    #[test]
    fn env_overrides_and_defaults_apply() {
        harness().run(|jail| {
            jail.env_key("database.url", "postgres://localhost/tankovault");
            jail.env_key("database.max_connections", 32);

            let cfg: Sample = load()?;
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

    /// A mounted secret outranks the TOML layer, so a `ConfigMap` carrying a placeholder DSN
    /// cannot win over the `Secret` that carries the real one.
    ///
    /// The test asserts the layer as well as the value, because a value the TOML duplicated would
    /// match from the wrong source.
    #[test]
    fn a_secrets_directory_outranks_the_toml_layer() {
        harness().run(|jail| {
            jail.config("[database]\nurl = \"postgres://placeholder/tv\"\n")?;
            jail.secret_key("database.url", "postgres://real/tv\n")?;

            let cfg: Sample = load()?;
            assert_eq!(cfg.database.url.expose_secret(), "postgres://real/tv");

            let explanation = super::explain()?;
            let origin = explanation
                .origin("database.url")
                .expect("the loaded key is reported");
            assert!(
                matches!(origin.effective(), Layer::SecretsFile(_)),
                "the mounted file must be the effective source: {origin:?}"
            );
            Ok(())
        });
    }

    /// The two process-level keys are reserved, so a mounted file naming one fails the boot
    /// instead of being read by nobody.
    ///
    /// `TANKOVAULT_PROFILE` decides whether the production guards run, and
    /// [`super::is_production`] reads it before a figment exists. A `Secret` supplying it would
    /// set a profile no guard sees.
    ///
    /// The file name is spelled out, because the name is what the loader must refuse.
    #[test]
    fn a_process_level_key_cannot_come_from_a_file() {
        harness().run(|jail| {
            jail.secret("profile", "production")?;

            let error = load::<Sample>().expect_err("a file may not supply TANKOVAULT_PROFILE");
            assert!(
                error.to_string().contains("TANKOVAULT_PROFILE"),
                "the error must name the key: {error}"
            );
            Ok(())
        });
    }
}
