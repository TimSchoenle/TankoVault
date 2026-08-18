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
/// Returns [`ConfigError`] if a required value is missing, a value fails to parse, a
/// file-backed source cannot be read, or one key is supplied by more than one of the last
/// three layers.
pub fn load<T: DeserializeOwned>() -> Result<T, ConfigError> {
    terrace().load()
}

/// Load a typed config and everything a reload needs to load it again.
///
/// # Errors
/// As [`load`].
pub fn load_watched<T: DeserializeOwned>() -> Result<Loaded<T>, ConfigError> {
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
