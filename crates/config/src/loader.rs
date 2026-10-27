//! The `TankoVault` dialect of [`terrace_config`], and the shared `serde`-default helpers used
//! across this crate.
//!
//! The layering itself — the TOML fragments, the `TANKOVAULT_*` environment layer, the
//! secrets-directory provider, the `_FILE` indirection and the shadow-key rejection — belongs
//! to `terrace-config`. What stays here is the one thing that is ours: which environment names
//! this deployment spells, and which of them a file may not supply.

use serde::de::DeserializeOwned;
use terrace_config::Terrace;

pub use terrace_config::explain::{Explanation, Layer};
pub use terrace_config::{Error as ConfigError, Loaded, Sources};

/// The prefix every configuration variable carries.
const PREFIX: &str = "TANKOVAULT_";

/// The loader every service boots through.
///
/// Layers, lowest precedence first: struct defaults, TOML at `$TANKOVAULT_CONFIG` (a file, or
/// every `*.toml` in it if it names a directory), `TANKOVAULT_`-prefixed `__`-nested
/// environment variables, `$TANKOVAULT_SECRETS_DIR`, and `TANKOVAULT_<KEY>_FILE` indirection.
/// The last three are mutually exclusive per key: a key supplied by two of them is refused at
/// boot rather than resolved by precedence.
///
/// A reserved key is read straight from the environment, before or outside the layered config,
/// so a file may not supply one — naming it from a secrets directory or a `_FILE` variable is
/// refused rather than ignored, because an ignored key is exactly the silent misconfiguration
/// the file layers exist to remove. `TANKOVAULT_CONFIG` and `TANKOVAULT_SECRETS_DIR` are
/// reserved by `terrace-config` itself, since both are read to decide what the layers *are*.
///
/// Every name below is a **literal on purpose**, including the two `Terrace::new(PREFIX)` would
/// have derived anyway: `xtask config-docs` reads the documented surface out of the literals in
/// this tree, so a variable that exists only as a derivation inside a dependency, or only as an
/// element of a `const` array, is one no row of `docs/CONFIGURATION.md` can be held to.
#[must_use]
pub fn terrace() -> Terrace {
    Terrace::new(PREFIX)
        .config_var("TANKOVAULT_CONFIG")
        .secrets_dir_var("TANKOVAULT_SECRETS_DIR")
        // Read by `is_production`, which every production guard is spelled in terms of.
        .reserve("TANKOVAULT_PROFILE")
        // Read by `xtask reset`, which never builds a figment at all.
        .reserve("TANKOVAULT_CONFIRM_RESET")
}

/// Load a typed config for a service.
///
/// # Errors
/// Returns [`ConfigError`] if `TANKOVAULT_PROFILE` names no known profile, a required value is
/// missing, a value fails to parse, a file-backed source cannot be read, or one key is supplied
/// by more than one of the last three layers.
pub fn load<T: DeserializeOwned>() -> Result<T, ConfigError> {
    Profile::from_env()?;
    terrace().load()
}

/// Load a typed config and everything a reload needs to load it again.
///
/// # Errors
/// As [`load`].
pub fn load_watched<T: DeserializeOwned>() -> Result<Loaded<T>, ConfigError> {
    Profile::from_env()?;
    terrace().load_watched()
}

/// Report which layer supplied each key, without loading a config.
///
/// Re-reads the layers at the moment it is called, so it is as valid inside a reload as at
/// boot. It carries **no configuration value** — only key paths and the layers they came from
/// — which is what makes it safe to log, and `tankovault-service`'s reload module is the one
/// caller that does.
///
/// # Errors
/// As [`load`], except that a key supplied by more than one layer is reported rather than
/// refused: an explanation of a configuration that fails to load is the case worth having.
pub fn explain() -> Result<Explanation, ConfigError> {
    terrace().explain()
}

/// Shared `serde` default for fields that are on unless explicitly disabled.
///
/// One spelling so no service re-derives it wrong (`cookie_secure` once defaulted to *off*).
#[must_use]
pub fn default_true() -> bool {
    true
}

/// The deployment profile named by `TANKOVAULT_PROFILE`.
///
/// Parsed in exactly one place. Two hand-written checks once disagreed on `prod` and on letter
/// case, so one value switched the API's secret checks on while leaving internal auth off, and
/// another did the reverse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    /// Unset, `development` or `dev`.
    Development,
    /// `production` or `prod`.
    Production,
}

impl Profile {
    /// Parse a raw `TANKOVAULT_PROFILE` value, trimmed and case-insensitive.
    ///
    /// # Errors
    /// [`ConfigError::Invalid`] for any other value: a profile nobody recognises must stop the
    /// boot rather than quietly mean "development".
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

    /// The profile this process runs under.
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

/// Whether the process runs under the production profile; every production guard is spelled in
/// terms of this.
///
/// An unrecognised profile answers `true`, so a guard consulted before [`load`] has refused the
/// boot fails closed.
#[must_use]
pub fn is_production() -> bool {
    Profile::from_env().map_or(true, |p| p == Profile::Production)
}

#[cfg(test)]
mod profile_tests {
    use super::Profile;

    /// `prod` and letter case used to be read differently by the API and the config crate, so a
    /// single value turned one set of production guards on and the other off.
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

    /// `staging` used to mean "development" silently, disarming every production guard.
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
    /// [`super::terrace`] reads — a variable spelled out by hand would keep passing after the
    /// prefix moved, while testing something nothing reads.
    fn harness() -> Harness {
        Harness::over(super::terrace())
    }

    /// The dialect, end to end: the prefix, the `__` nesting, and the defaults that fill in
    /// around what the environment supplied. `terrace-config` owns the layering and tests it;
    /// what these pin is that this crate wires it to the names an operator actually sets.
    ///
    /// `load()` rather than `jail.load()` throughout, because the entry point every service
    /// boots through is half of what is under test here.
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
    /// cannot win over the `Secret` that carries the real one — through the variable names
    /// *this* crate configures, which is the half a dependency cannot pin.
    ///
    /// The layer is asserted as well as the value: the two spellings differ, so a `secret` the
    /// TOML happened to duplicate would produce the expected string from the wrong source and
    /// pin nothing.
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
    /// `TANKOVAULT_PROFILE` decides whether the production guards run at all, and it is read by
    /// [`super::is_production`] long before a figment exists. A `Secret` supplying it would have
    /// left a deployment believing it had set the profile while every guard saw it unset.
    ///
    /// The file name is spelled verbatim on purpose: what is under test is a *name* the loader
    /// must refuse, so deriving it from the key would beg the question.
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
