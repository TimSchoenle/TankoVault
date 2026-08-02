//! Assembles the layered figment every service boots through; also the shared `serde`-default
//! helpers used across this crate.

use std::path::{Path, PathBuf};

use figment::{
    Figment,
    providers::{Env, Format, Toml},
};
use serde::de::DeserializeOwned;

use crate::error::ConfigError;
use crate::secrets::FileLayers;

/// Load a typed config for a service.
///
/// Layers, lowest precedence first: struct defaults, TOML at `$TANKOVAULT_CONFIG` (a file, or
/// every `*.toml` in it if it names a directory), `TANKOVAULT_`-prefixed `__`-nested
/// environment variables, `$TANKOVAULT_SECRETS_DIR`, and `TANKOVAULT_<KEY>_FILE` indirection.
///
/// # Errors
/// Returns [`ConfigError`] if a required value is missing, a value fails to parse, a
/// file-backed source cannot be read, or one key is supplied by more than one of the last
/// three layers.
pub fn load<T: DeserializeOwned>() -> Result<T, ConfigError> {
    assemble()?.0.extract().map_err(|e| Box::new(e).into())
}

/// A loaded config together with what a reload needs to watch and to compare against.
#[derive(Debug, Clone)]
pub struct Loaded<T> {
    /// The extracted config.
    pub value: T,
    /// Where it came from.
    pub sources: Sources,
}

/// The filesystem inputs a config was assembled from, and a fingerprint of the result.
///
/// The fingerprint is the fully merged figment value rather than the typed config: several
/// config structs hold `SecretString`, which deliberately has no `PartialEq`, so the typed
/// value cannot be compared. Comparing the merged value instead means a reload that changes
/// nothing — a `ConfigMap` rewritten with identical contents, a `..data` swap that moved no
/// key — is detected as a no-op before anything is torn down and rebuilt.
#[derive(Debug, Clone)]
pub struct Sources {
    watch: Vec<PathBuf>,
    fingerprint: figment::value::Value,
}

impl Sources {
    /// Directories to watch for changes. Directories, not files: a Kubernetes volume update
    /// renames a whole new `..data` directory over the old one, so a watch registered against
    /// a file's inode never fires a second time.
    #[must_use]
    pub fn watch_paths(&self) -> &[PathBuf] {
        &self.watch
    }

    /// Whether `self` resolves to different values than `previous`.
    #[must_use]
    pub fn differs_from(&self, previous: &Self) -> bool {
        self.fingerprint != previous.fingerprint
    }
}

/// Load a typed config and everything needed to load it again later.
///
/// # Errors
/// As [`load`].
pub fn load_watched<T: DeserializeOwned>() -> Result<Loaded<T>, ConfigError> {
    let (figment, files) = assemble()?;
    let fingerprint = figment
        .extract::<figment::value::Value>()
        .map_err(Box::new)?;
    let value = figment.extract().map_err(Box::new)?;

    let mut watch: Vec<PathBuf> = files.watch_paths().into_iter().collect();
    let toml = std::env::var("TANKOVAULT_CONFIG").unwrap_or_else(|_| "config.toml".to_owned());
    let toml = PathBuf::from(toml);
    // The directory either way: watching a `config.toml` that does not exist yet registers
    // nothing, and a file created later would then never be noticed.
    let toml_dir = if toml.is_dir() {
        Some(toml)
    } else {
        toml.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_path_buf)
    };
    watch.extend(toml_dir);
    watch.sort();
    watch.dedup();

    Ok(Loaded {
        value,
        sources: Sources { watch, fingerprint },
    })
}

/// The assembled figment, plus the file layers that went into it.
///
/// Split out from [`load`] because a reload has to re-run exactly this assembly, and needs the
/// [`FileLayers`] to know which paths to watch.
///
/// # Errors
/// As [`load`], minus the extraction.
pub(crate) fn assemble() -> Result<(Figment, FileLayers), ConfigError> {
    let path = std::env::var("TANKOVAULT_CONFIG").unwrap_or_else(|_| "config.toml".to_owned());
    let mut figment = Figment::new();
    for toml in toml_layers(Path::new(&path))? {
        figment = figment.merge(Toml::file(toml));
    }
    figment = figment.merge(Env::prefixed("TANKOVAULT_").split("__"));

    // Collected after the environment layer is in place but merged on top of it: the file
    // layers are the more deliberate way to supply a value, and `FileLayers::collect` has
    // already refused any key that both mechanisms define.
    let files = FileLayers::collect()?;
    if !files.is_empty() {
        figment = figment.merge(files.clone());
    }
    Ok((figment, files))
}

/// The TOML files `$TANKOVAULT_CONFIG` denotes, in merge order.
///
/// A plain path is returned as-is, missing or not — `Toml::file` skips an absent file, and
/// that "a misspelled path is not an error" behaviour predates this function. A *directory* is
/// expanded to every `*.toml` directly inside it, sorted by name so a `10-base.toml` /
/// `20-overrides.toml` pair merges in the order an operator reading the mount would predict.
/// Dot-prefixed entries are skipped for the same reason as in [`crate::secrets`]: a Kubernetes
/// `ConfigMap` volume is a directory of symlinks beside a `..data` directory.
fn toml_layers(path: &Path) -> Result<Vec<PathBuf>, ConfigError> {
    if !path.is_dir() {
        return Ok(vec![path.to_path_buf()]);
    }

    let entries = std::fs::read_dir(path).map_err(|e| {
        ConfigError::Source(format!(
            "TANKOVAULT_CONFIG is {}, which could not be read: {e}",
            path.display()
        ))
    })?;

    let mut files = Vec::new();
    for entry in entries {
        let entry =
            entry.map_err(|e| ConfigError::Source(format!("reading {}: {e}", path.display())))?;
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        if !entry.metadata().is_ok_and(|m| m.is_file()) {
            continue;
        }
        let file = entry.path();
        if file
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("toml"))
        {
            files.push(file);
        }
    }
    files.sort();
    Ok(files)
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

    /// `$TANKOVAULT_CONFIG` naming a directory merges every `*.toml` in it, later name winning
    /// — a `ConfigMap` mounted as a directory of fragments.
    #[test]
    #[expect(
        clippy::result_large_err,
        reason = "figment::Jail::expect_with fixes the closure's error type"
    )]
    fn a_toml_directory_merges_its_fragments_in_name_order() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.create_dir("conf.d")?;
            jail.create_file(
                "conf.d/10-base.toml",
                "[database]\nurl = \"postgres://base/tv\"\nmax_connections = 5\n",
            )?;
            jail.create_file(
                "conf.d/20-overrides.toml",
                "[database]\nmax_connections = 40\n",
            )?;
            // Neither is a `*.toml`, so neither may contribute; the dot-prefixed one is also
            // what a `ConfigMap` volume puts beside the real keys.
            jail.create_file("conf.d/..data", "[database]\nmax_connections = 1\n")?;
            jail.create_file("conf.d/notes.md", "not config\n")?;
            jail.set_env(
                "TANKOVAULT_CONFIG",
                jail.directory().join("conf.d").display(),
            );

            let cfg: Sample = load().map_err(|e| e.to_string()).unwrap();
            assert_eq!(cfg.database.url.expose_secret(), "postgres://base/tv");
            assert_eq!(cfg.database.max_connections, 40);
            Ok(())
        });
    }

    /// A mounted secret outranks the TOML layer, so a `ConfigMap` carrying a placeholder DSN
    /// cannot win over the `Secret` that carries the real one.
    #[test]
    #[expect(
        clippy::result_large_err,
        reason = "figment::Jail::expect_with fixes the closure's error type"
    )]
    fn a_secrets_directory_outranks_the_toml_layer() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.create_file(
                "config.toml",
                "[database]\nurl = \"postgres://placeholder/tv\"\n",
            )?;
            jail.create_dir("secrets")?;
            jail.create_file("secrets/database__url", "postgres://real/tv\n")?;
            jail.set_env(
                "TANKOVAULT_CONFIG",
                jail.directory().join("config.toml").display(),
            );
            jail.set_env(
                "TANKOVAULT_SECRETS_DIR",
                jail.directory().join("secrets").display(),
            );

            let cfg: Sample = load().map_err(|e| e.to_string()).unwrap();
            assert_eq!(cfg.database.url.expose_secret(), "postgres://real/tv");
            Ok(())
        });
    }

    /// A reload that changes nothing must be detectable as a no-op: a `..data` swap that moved
    /// no key would otherwise rebuild the pool and rebind the listener for nothing.
    #[test]
    #[expect(
        clippy::result_large_err,
        reason = "figment::Jail::expect_with fixes the closure's error type"
    )]
    fn the_fingerprint_tracks_values_not_reads() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.create_dir("secrets")?;
            jail.create_file("secrets/database__url", "postgres://one/tv")?;
            jail.set_env(
                "TANKOVAULT_SECRETS_DIR",
                jail.directory().join("secrets").display(),
            );

            let first = super::load_watched::<Sample>()
                .map_err(|e| e.to_string())
                .unwrap();
            let again = super::load_watched::<Sample>()
                .map_err(|e| e.to_string())
                .unwrap();
            assert!(
                !again.sources.differs_from(&first.sources),
                "re-reading unchanged files must not look like a change"
            );

            jail.create_file("secrets/database__url", "postgres://two/tv")?;
            let rotated = super::load_watched::<Sample>()
                .map_err(|e| e.to_string())
                .unwrap();
            assert!(
                rotated.sources.differs_from(&first.sources),
                "a rotated secret must look like a change"
            );
            assert!(
                rotated
                    .sources
                    .watch_paths()
                    .iter()
                    .any(|p| p.ends_with("secrets")),
                "the secrets directory must be watched: {:?}",
                rotated.sources.watch_paths()
            );
            Ok(())
        });
    }
}
