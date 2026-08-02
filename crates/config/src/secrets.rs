//! Configuration values that arrive as files rather than as environment variables.
//!
//! Two mechanisms, both shaped for Kubernetes: a directory of key-named files
//! (`$TANKOVAULT_SECRETS_DIR` — what a `Secret` mounted as a volume looks like) and per-key
//! indirection (`TANKOVAULT_<KEY>_FILE=/path` — what Docker Compose `secrets:` looks like).
//!
//! A value in a pod's environment is readable from `/proc/<pid>/environ` by anything sharing
//! the namespace, is inherited by every child process, and is printed by anything that dumps
//! the environment. A mounted file is none of those things, and the kubelet updates it in
//! place when the `Secret` changes, which is what lets a running service pick up a rotated
//! credential without being restarted (`tankovault_service::reload`).
//!
//! **Values from these layers are emitted unparsed, as strings.** `figment::providers::Env`
//! runs a TOML-ish parse over every value, so `12345678` becomes a number — and
//! `Figment::extract` uses figment's default interpreter, which will not coerce a number back
//! into a string, so an all-digit password supplied that way fails to deserialise into
//! `SecretString`. These layers exist to carry secrets, and a secret is an opaque byte string.
//! Anything structured belongs in the TOML layer, which parses it properly.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use figment::value::{Dict, Map, Value};
use figment::{Metadata, Profile, Provider};

use crate::error::ConfigError;

/// The prefix every configuration key carries, in its environment spelling.
const PREFIX: &str = "TANKOVAULT_";

/// The suffix marking an environment variable that names a *file* holding a value rather than
/// holding the value itself.
const FILE_SUFFIX: &str = "_FILE";

/// Keys read straight from the environment, before or outside the layered config.
///
/// A file cannot supply one: `TANKOVAULT_CONFIG` and `TANKOVAULT_SECRETS_DIR` are read to
/// decide what the layers *are*, and `TANKOVAULT_PROFILE`/`TANKOVAULT_CONFIRM_RESET` are read
/// by callers that never build a figment. Naming one from a file is refused rather than
/// ignored, because an ignored key is exactly the silent misconfiguration these layers are
/// meant to remove.
const PROCESS_KEYS: [&str; 4] = [
    "TANKOVAULT_CONFIG",
    "TANKOVAULT_PROFILE",
    "TANKOVAULT_SECRETS_DIR",
    "TANKOVAULT_CONFIRM_RESET",
];

/// One value and the file it came from.
///
/// The path is kept so an error can name its source. No error in this module ever prints the
/// value: the whole point of the layer is that the value stays out of anything ambient.
#[derive(Debug, Clone)]
struct FileValue {
    /// The file this value was read from.
    path: PathBuf,
    /// The contents, minus trailing line terminators.
    value: String,
}

/// The file-backed layers, collected together so the shadowing check can see all of them at
/// once.
#[derive(Debug, Clone, Default)]
pub(crate) struct FileLayers {
    /// Values from `$TANKOVAULT_SECRETS_DIR`, keyed by figment key path.
    secrets_dir: BTreeMap<String, FileValue>,
    /// Values from `TANKOVAULT_<KEY>_FILE` indirection, keyed by figment key path.
    file_suffix: BTreeMap<String, FileValue>,
    /// The secrets directory itself, when one was configured. Retained for [`Self::watch_paths`].
    dir: Option<PathBuf>,
}

impl FileLayers {
    /// Read every file-backed layer the environment points at.
    ///
    /// # Errors
    /// Returns [`ConfigError::Source`] if a configured secrets directory or `_FILE` path
    /// cannot be read, if a file's contents are not UTF-8, if a file name is not a usable key,
    /// or if one key is supplied by more than one mechanism.
    pub(crate) fn collect() -> Result<Self, ConfigError> {
        let dir = std::env::var("TANKOVAULT_SECRETS_DIR")
            .ok()
            .filter(|d| !d.trim().is_empty())
            .map(PathBuf::from);

        let layers = Self {
            secrets_dir: match dir.as_deref() {
                Some(dir) => read_secrets_dir(dir)?,
                None => BTreeMap::new(),
            },
            file_suffix: read_file_suffix_env()?,
            dir,
        };
        layers.reject_shadowed_keys()?;
        Ok(layers)
    }

    /// Whether any file-backed value was found. Callers skip the providers entirely when not.
    pub(crate) fn is_empty(&self) -> bool {
        self.secrets_dir.is_empty() && self.file_suffix.is_empty()
    }

    /// The paths a reload has to watch: the secrets directory, and every `_FILE` target.
    ///
    /// Parent directories rather than the files themselves for the `_FILE` targets — a
    /// Kubernetes volume update replaces the file by renaming a new `..data` directory over
    /// the old one, so a watch registered against the old inode never fires again.
    pub(crate) fn watch_paths(&self) -> BTreeSet<PathBuf> {
        let mut paths = BTreeSet::new();
        if let Some(dir) = &self.dir {
            paths.insert(dir.clone());
        }
        for value in self.file_suffix.values() {
            if let Some(parent) = value.path.parent() {
                paths.insert(parent.to_path_buf());
            }
        }
        paths
    }

    /// Refuse a key supplied by more than one of: the environment, the secrets directory, the
    /// `_FILE` indirection.
    ///
    /// Precedence would be the softer option, and it is the wrong one here. The failure this
    /// prevents is a half-migrated deployment where a stale environment variable shadows a
    /// mounted secret that has since been rotated — the service keeps working, with the old
    /// credential, and the discrepancy surfaces during an incident rather than during a deploy.
    fn reject_shadowed_keys(&self) -> Result<(), ConfigError> {
        let env = plain_env_keys();
        for (key, value) in &self.secrets_dir {
            if let Some(other) = self.file_suffix.get(key) {
                return Err(shadowed(key, &value.path.display(), &other.path.display()));
            }
            if env.contains(key) {
                return Err(shadowed(key, &value.path.display(), &env_spelling(key)));
            }
        }
        for (key, value) in &self.file_suffix {
            if env.contains(key) {
                return Err(shadowed(key, &value.path.display(), &env_spelling(key)));
            }
        }
        Ok(())
    }
}

impl Provider for FileLayers {
    fn metadata(&self) -> Metadata {
        Metadata::named("file-backed configuration")
    }

    fn data(&self) -> Result<Map<Profile, Dict>, figment::Error> {
        let mut dict = Dict::new();
        // The secrets directory first so `_FILE` wins on the merge. They cannot actually
        // collide — `reject_shadowed_keys` already refused that — but the ordering makes the
        // provider correct on its own rather than only in the context that validates it.
        for (key, value) in self.secrets_dir.iter().chain(&self.file_suffix) {
            insert_nested(&mut dict, key, Value::from(value.value.as_str()));
        }
        Ok(Profile::Default.collect(dict))
    }
}

/// Read every key-named file directly inside `dir`.
///
/// Entries whose name begins with `.` are skipped, and so is anything that is not a regular
/// file. Both are what makes a Kubernetes projected `Secret` volume work: it holds a `..data`
/// symlink pointing at a timestamped directory, plus one symlink per key. The per-key symlinks
/// must be followed, which is why this uses `fs::metadata` and **not** `DirEntry::metadata()`:
/// despite the name the latter does *not* traverse symlinks — it carries `symlink_metadata`
/// semantics — so it classifies every real key as "not a file" and silently yields an empty
/// layer. That is not hypothetical: it is what shipped, and every service then booted on
/// compiled defaults and died naming the first required field it was missing.
fn read_secrets_dir(dir: &Path) -> Result<BTreeMap<String, FileValue>, ConfigError> {
    let entries = std::fs::read_dir(dir).map_err(|e| {
        ConfigError::Source(format!(
            "TANKOVAULT_SECRETS_DIR is {}, which could not be read: {e}",
            dir.display()
        ))
    })?;

    let mut values = BTreeMap::new();
    for entry in entries {
        let entry =
            entry.map_err(|e| ConfigError::Source(format!("reading {}: {e}", dir.display())))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        // Follows symlinks; see the note above. A dangling link yields `Err` and is skipped,
        // which is the same outcome as before for a genuinely absent target.
        let is_file = std::fs::metadata(&path).is_ok_and(|m| m.is_file());
        if !is_file {
            continue;
        }

        let key = key_from_name(&name, &path)?;
        values.insert(
            key,
            FileValue {
                value: read_value(&path)?,
                path,
            },
        );
    }
    Ok(values)
}

/// Read every `TANKOVAULT_<KEY>_FILE` indirection the environment declares.
///
/// Scanned over the whole environment rather than by `env::var("…")` on a literal, both
/// because the keys are open-ended and because `xtask config-docs` derives the documented
/// surface from `env::var` literals — a literal here would put a key in the derived surface
/// that no table row can honestly claim.
fn read_file_suffix_env() -> Result<BTreeMap<String, FileValue>, ConfigError> {
    let mut values = BTreeMap::new();
    for (name, path) in std::env::vars_os() {
        let (Some(name), Some(path)) = (name.to_str(), path.to_str()) else {
            continue;
        };
        let Some(key) = name
            .strip_prefix(PREFIX)
            .and_then(|k| k.strip_suffix(FILE_SUFFIX))
        else {
            continue;
        };
        if key.is_empty() {
            continue;
        }

        let spelled = format!("{PREFIX}{key}");
        if PROCESS_KEYS.contains(&spelled.as_str()) {
            return Err(ConfigError::Source(format!(
                "{name} is set, but {spelled} is read directly from the environment before the \
                 layered config is built, so a file cannot supply it. Set {spelled} itself."
            )));
        }

        let path = PathBuf::from(path);
        // A `_FILE` naming an unreadable path is fatal rather than skipped. Skipping is how a
        // secret goes silently unset and the service boots with a default instead.
        let value = read_value(&path)
            .map_err(|e| ConfigError::Source(format!("{name} names {}: {e}", path.display())))?;
        values.insert(normalise_key(key), FileValue { path, value });
    }
    Ok(values)
}

/// The contents of one value file, minus trailing line terminators.
///
/// Only `\r` and `\n` are stripped, never spaces or tabs: `printf 'x\n' > f` and every text
/// editor add a newline nobody meant as part of the value, whereas a trailing space can be a
/// real character of a real password.
fn read_value(path: &Path) -> Result<String, ConfigError> {
    let bytes = std::fs::read(path)
        .map_err(|e| ConfigError::Source(format!("reading {}: {e}", path.display())))?;
    // Named, not printed: the file holds a secret, so the invalid bytes stay out of the log.
    let text = String::from_utf8(bytes)
        .map_err(|_| ConfigError::Source(format!("{} is not valid UTF-8", path.display())))?;
    Ok(text.trim_end_matches(['\r', '\n']).to_owned())
}

/// The figment key a secrets-directory file name denotes.
///
/// # Errors
/// A name containing `.` — Kubernetes allows it in a `Secret` key, but the nesting separator
/// here is `__`, so `auth.jwt_secret` would silently mean something other than it looks like.
fn key_from_name(name: &str, path: &Path) -> Result<String, ConfigError> {
    if name.contains('.') {
        return Err(ConfigError::Source(format!(
            "{} is not a usable key: `.` is not the nesting separator, `__` is (`auth__jwt_secret` \
             for `auth.jwt_secret`). Rename the entry, or move the file out of the secrets \
             directory.",
            path.display()
        )));
    }

    let spelled = format!("{PREFIX}{}", name.to_ascii_uppercase());
    if PROCESS_KEYS.contains(&spelled.as_str()) {
        return Err(ConfigError::Source(format!(
            "{} names {spelled}, which is read directly from the environment before the layered \
             config is built, so a file cannot supply it.",
            path.display()
        )));
    }
    Ok(normalise_key(name))
}

/// An environment key suffix (`AUTH__JWT_SECRET`) as a figment key path (`auth.jwt_secret`).
///
/// The one spelling both layers use, so a file and an environment variable cannot disagree
/// about which field they name.
fn normalise_key(suffix: &str) -> String {
    suffix
        .to_ascii_lowercase()
        .split("__")
        .collect::<Vec<_>>()
        .join(".")
}

/// A figment key path back in its environment spelling, for error messages.
fn env_spelling(key: &str) -> String {
    format!("{PREFIX}{}", key.to_ascii_uppercase().replace('.', "__"))
}

/// Every key the environment supplies *directly* — excluding the `_FILE` indirections, which
/// are the mechanism rather than a value, and the process-level keys, which are not part of
/// the layered config at all.
fn plain_env_keys() -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    for (name, _) in std::env::vars_os() {
        let Some(name) = name.to_str() else { continue };
        if PROCESS_KEYS.contains(&name) {
            continue;
        }
        // `auth.jwt_secret_file` is what figment makes of the indirection variable — an
        // unknown key it ignores. It must not be mistaken for `auth.jwt_secret`, which is what
        // the indirection actually supplies.
        if name.ends_with(FILE_SUFFIX) {
            continue;
        }
        if let Some(suffix) = name.strip_prefix(PREFIX)
            && !suffix.is_empty()
        {
            keys.insert(normalise_key(suffix));
        }
    }
    keys
}

/// The one shadowing error, so both directions read identically.
fn shadowed(
    key: &str,
    source: &impl std::fmt::Display,
    other: &impl std::fmt::Display,
) -> ConfigError {
    ConfigError::Source(format!(
        "`{key}` is supplied twice — by {source} and by {other}. Remove one: a stale \
         environment variable shadowing a rotated secret keeps the service running on the old \
         credential."
    ))
}

/// Insert `value` at a dot-separated `key` path, creating intermediate dictionaries.
///
/// Written out rather than using `figment::util::nest` because that returns one nested `Value`
/// per key and merging them needs figment's private `Coalescible`.
fn insert_nested(dict: &mut Dict, key: &str, value: Value) {
    match key.split_once('.') {
        None => {
            dict.insert(key.to_owned(), value);
        }
        Some((head, rest)) => {
            let entry = dict
                .entry(head.to_owned())
                .or_insert_with(|| Value::Dict(figment::value::Tag::Default, Dict::new()));
            // A non-dict here means two keys disagree about whether a segment is a leaf
            // (`a` and `a__b`). The later one wins by replacing the leaf, which is what the
            // environment layer does too.
            if !matches!(entry, Value::Dict(..)) {
                *entry = Value::Dict(figment::value::Tag::Default, Dict::new());
            }
            if let Value::Dict(_, inner) = entry {
                insert_nested(inner, rest, value);
            }
        }
    }
}

#[cfg(test)]
#[expect(
    clippy::result_large_err,
    reason = "figment::Jail::expect_with fixes the closure's error type to figment::Error"
)]
mod tests {
    use crate::load;
    use secrecy::{ExposeSecret as _, SecretString};
    use serde::Deserialize;

    /// Two levels of nesting, and a `SecretString` leaf — the shape every real secret has.
    #[derive(Debug, Deserialize)]
    struct Sample {
        auth: Auth,
    }

    #[derive(Debug, Deserialize)]
    struct Auth {
        jwt_secret: SecretString,
    }

    /// A jail with no inherited `TANKOVAULT_*` variables: several of these assertions are about
    /// what the environment does *not* contain, and a developer machine with a real value
    /// exported would otherwise decide the outcome.
    fn jailed(f: impl FnOnce(&mut figment::Jail) -> figment::error::Result<()>) {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            f(jail)
        });
    }

    fn secrets_dir(jail: &figment::Jail) -> std::path::PathBuf {
        jail.directory().join("secrets")
    }

    /// A symlink, as the error type `figment::Jail` closures return: it has no
    /// `From<std::io::Error>`, and `Jail` itself can only create regular files and directories.
    #[cfg(unix)]
    fn symlink(target: &str, link: &std::path::Path) -> figment::error::Result<()> {
        std::os::unix::fs::symlink(target, link).map_err(|e| {
            figment::Error::from(format!("symlinking {} -> {target}: {e}", link.display()))
        })
    }

    #[test]
    fn a_secrets_directory_file_supplies_a_nested_key() {
        jailed(|jail| {
            jail.create_dir("secrets")?;
            // A trailing newline, which is what `printf '%s\n'` and every editor produce.
            jail.create_file("secrets/auth__jwt_secret", "s3cret\n")?;
            jail.set_env("TANKOVAULT_SECRETS_DIR", secrets_dir(jail).display());

            let cfg: Sample = load().map_err(|e| e.to_string()).unwrap();
            assert_eq!(cfg.auth.jwt_secret.expose_secret(), "s3cret");
            Ok(())
        });
    }

    /// Interior and leading whitespace survive; only trailing line terminators are stripped.
    /// A trailing space can be a real character of a real password, so trimming it would
    /// corrupt a credential silently rather than fail.
    #[test]
    fn only_trailing_line_terminators_are_stripped() {
        jailed(|jail| {
            jail.create_dir("secrets")?;
            jail.create_file("secrets/auth__jwt_secret", " pass phrase \r\n\n")?;
            jail.set_env("TANKOVAULT_SECRETS_DIR", secrets_dir(jail).display());

            let cfg: Sample = load().map_err(|e| e.to_string()).unwrap();
            assert_eq!(cfg.auth.jwt_secret.expose_secret(), " pass phrase ");
            Ok(())
        });
    }

    /// The layout a Kubernetes `Secret` volume actually has: a `..data` entry and a timestamped
    /// directory beside the real keys. Reading either as a key yields a garbage config entry;
    /// classifying the real keys with `symlink_metadata` instead of `metadata` yields an empty
    /// layer and a service that boots on defaults. Both were live hazards, so this pins the
    /// shape rather than the rule.
    #[test]
    fn a_projected_volume_layout_yields_only_the_real_keys() {
        jailed(|jail| {
            jail.create_dir("secrets")?;
            jail.create_dir("secrets/..2026_08_02_10_00_00")?;
            jail.create_file("secrets/..data", "not a key")?;
            jail.create_dir("secrets/nested")?;
            jail.create_file("secrets/auth__jwt_secret", "from-the-volume")?;
            jail.set_env("TANKOVAULT_SECRETS_DIR", secrets_dir(jail).display());

            let cfg: Sample = load().map_err(|e| e.to_string()).unwrap();
            assert_eq!(cfg.auth.jwt_secret.expose_secret(), "from-the-volume");
            Ok(())
        });
    }

    /// The same layout as above, built the way the kubelet actually builds it: the keys are
    /// **symlinks** into `..data`, not regular files.
    ///
    /// The test above pins the volume's *names* but writes the keys as real files, and that gap
    /// is exactly what let the symlink bug ship — it stayed green while every service in the
    /// cluster booted on compiled defaults, because `DirEntry::metadata()` reports a symlink as
    /// "not a file" and the whole layer came back empty. Only a real symlink reproduces it.
    #[cfg(unix)]
    #[test]
    fn keys_symlinked_into_dot_data_are_read() {
        jailed(|jail| {
            let data = "..2026_08_02_10_00_00";
            jail.create_dir("secrets")?;
            jail.create_dir(format!("secrets/{data}"))?;
            jail.create_file(
                format!("secrets/{data}/auth__jwt_secret"),
                "from-the-volume",
            )?;

            let dir = secrets_dir(jail);
            symlink(data, &dir.join("..data"))?;
            symlink("..data/auth__jwt_secret", &dir.join("auth__jwt_secret"))?;
            jail.set_env("TANKOVAULT_SECRETS_DIR", dir.display());

            let cfg: Sample = load().map_err(|e| e.to_string()).unwrap();
            assert_eq!(cfg.auth.jwt_secret.expose_secret(), "from-the-volume");
            Ok(())
        });
    }

    #[test]
    fn file_suffix_indirection_supplies_a_key() {
        jailed(|jail| {
            jail.create_file("jwt", "from-the-path")?;
            jail.set_env(
                "TANKOVAULT_AUTH__JWT_SECRET_FILE",
                jail.directory().join("jwt").display(),
            );

            let cfg: Sample = load().map_err(|e| e.to_string()).unwrap();
            assert_eq!(cfg.auth.jwt_secret.expose_secret(), "from-the-path");
            Ok(())
        });
    }

    /// A `_FILE` naming a path that cannot be read fails the boot. Skipping it instead is how a
    /// secret goes silently unset and the service comes up on a default.
    #[test]
    fn a_file_suffix_path_that_cannot_be_read_is_fatal() {
        jailed(|jail| {
            jail.set_env(
                "TANKOVAULT_AUTH__JWT_SECRET_FILE",
                jail.directory().join("absent").display(),
            );

            let err = load::<Sample>().expect_err("an unreadable path must not be skipped");
            let message = err.to_string();
            assert!(
                message.contains("TANKOVAULT_AUTH__JWT_SECRET_FILE") && message.contains("absent"),
                "the error must name the variable and the path: {message}"
            );
            Ok(())
        });
    }

    /// A key supplied by both the environment and a mounted file is refused rather than
    /// resolved by precedence: the failure being prevented is a stale environment variable
    /// shadowing a rotated secret, where the service keeps working on the old credential and
    /// the discrepancy surfaces during an incident.
    #[test]
    fn a_key_supplied_twice_is_refused_and_no_value_is_printed() {
        jailed(|jail| {
            jail.create_dir("secrets")?;
            jail.create_file("secrets/auth__jwt_secret", "from-the-file")?;
            jail.set_env("TANKOVAULT_SECRETS_DIR", secrets_dir(jail).display());
            jail.set_env("TANKOVAULT_AUTH__JWT_SECRET", "from-the-environment");

            let err = load::<Sample>().expect_err("a shadowed key must be refused");
            let message = err.to_string();
            assert!(
                message.contains("auth.jwt_secret")
                    && message.contains("TANKOVAULT_AUTH__JWT_SECRET")
                    && message.contains("auth__jwt_secret"),
                "the error must name the key and both sources: {message}"
            );
            assert!(
                !message.contains("from-the-file") && !message.contains("from-the-environment"),
                "the error must not print either value: {message}"
            );
            Ok(())
        });
    }

    #[test]
    fn a_key_supplied_by_both_file_mechanisms_is_refused() {
        jailed(|jail| {
            jail.create_dir("secrets")?;
            jail.create_file("secrets/auth__jwt_secret", "a")?;
            jail.create_file("jwt", "b")?;
            jail.set_env("TANKOVAULT_SECRETS_DIR", secrets_dir(jail).display());
            jail.set_env(
                "TANKOVAULT_AUTH__JWT_SECRET_FILE",
                jail.directory().join("jwt").display(),
            );

            let err = load::<Sample>().expect_err("a shadowed key must be refused");
            assert!(err.to_string().contains("auth.jwt_secret"), "{err}");
            Ok(())
        });
    }

    /// `TANKOVAULT_AUTH__JWT_SECRET_FILE` is what figment's `Env` turns into the unknown key
    /// `auth.jwt_secret_file`. If the shadowing check confused that with `auth.jwt_secret` —
    /// the key the indirection actually supplies — then every `_FILE` variable would refuse
    /// its own value.
    #[test]
    fn the_indirection_variable_does_not_shadow_the_key_it_supplies() {
        jailed(|jail| {
            jail.create_file("jwt", "fine")?;
            jail.set_env(
                "TANKOVAULT_AUTH__JWT_SECRET_FILE",
                jail.directory().join("jwt").display(),
            );

            let cfg: Sample = load().map_err(|e| e.to_string()).unwrap();
            assert_eq!(cfg.auth.jwt_secret.expose_secret(), "fine");
            Ok(())
        });
    }

    /// A process-level key is read before the layered config exists, so a file naming one would
    /// be ignored. Ignoring is the silent misconfiguration these layers exist to remove.
    #[test]
    fn a_process_level_key_cannot_come_from_a_file() {
        jailed(|jail| {
            jail.create_file("profile", "production")?;
            jail.set_env(
                "TANKOVAULT_PROFILE_FILE",
                jail.directory().join("profile").display(),
            );

            let err = load::<Sample>().expect_err("a process-level key must be refused");
            assert!(err.to_string().contains("TANKOVAULT_PROFILE"), "{err}");
            Ok(())
        });
    }

    #[test]
    fn a_dotted_file_name_is_refused_rather_than_nested_wrongly() {
        jailed(|jail| {
            jail.create_dir("secrets")?;
            jail.create_file("secrets/auth.jwt_secret", "x")?;
            jail.set_env("TANKOVAULT_SECRETS_DIR", secrets_dir(jail).display());

            let err = load::<Sample>().expect_err("`.` is not the nesting separator");
            assert!(err.to_string().contains("__"), "{err}");
            Ok(())
        });
    }

    /// An all-digit secret reaches the config as a string.
    ///
    /// `figment::providers::Env` runs a TOML-ish parse over every value, so the same secret set
    /// as `TANKOVAULT_AUTH__JWT_SECRET=12345678` becomes a `Value::Num`, and figment's default
    /// interpreter will not coerce a number back into a string — the boot fails with "invalid
    /// type: integer, expected a string". The file layers emit values unparsed precisely so
    /// that a numeric password is not a deployment that cannot start.
    #[test]
    fn an_all_digit_secret_from_a_file_stays_a_string() {
        jailed(|jail| {
            jail.create_dir("secrets")?;
            jail.create_file("secrets/auth__jwt_secret", "12345678")?;
            jail.set_env("TANKOVAULT_SECRETS_DIR", secrets_dir(jail).display());

            let cfg: Sample = load().map_err(|e| e.to_string()).unwrap();
            assert_eq!(cfg.auth.jwt_secret.expose_secret(), "12345678");
            Ok(())
        });
    }

    /// A secrets directory the operator named but that is not there fails the boot: they said
    /// the secrets were mounted, and booting on defaults instead is the outcome worth avoiding.
    #[test]
    fn a_missing_secrets_directory_is_fatal() {
        jailed(|jail| {
            jail.set_env(
                "TANKOVAULT_SECRETS_DIR",
                jail.directory().join("absent").display(),
            );

            let err = load::<Sample>().expect_err("a named directory must exist");
            assert!(err.to_string().contains("TANKOVAULT_SECRETS_DIR"), "{err}");
            Ok(())
        });
    }
}
