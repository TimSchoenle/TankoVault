//! Owns rebuilding a running service when the files its configuration came from change.
//!
//! The supervisor itself is [`terrace_config::reload`]: the watcher, the debounce, the no-op
//! detection and the failure posture all live there. This module holds the one wiring decision
//! that is ours: the closure re-reading the configuration is [`tankovault_config::load_watched`],
//! so every service reloads through the same layers it booted through.
//!
//! # What is *not* rebuilt
//! The process-global installations that happen before [`run`] is reached and cannot be redone:
//! the `tracing` subscriber ([`crate::init_tracing`]) and the metrics recorder
//! ([`crate::MetricsRegistry::install`]). Changing `telemetry.*` or `metrics.*` still needs a
//! restart.
//!
//! # Failure posture
//! A reload that cannot be loaded, or that fails to build, leaves the running service exactly
//! as it was. The reload path runs code that at boot only ever ran under a scheduler that would
//! restart the container, so a bad file write must not be able to take down a healthy pod.

use std::future::Future;
use std::sync::Arc;

use serde::de::DeserializeOwned;
use tokio_util::sync::CancellationToken;

use tankovault_config::{ConfigError, Loaded};
use terrace_config::reload::WatchError;

/// Logs where the running configuration came from, and warns about every key a higher layer
/// is shadowing.
///
/// **It holds no configuration value**, only key paths, file names and layer names, which is
/// what makes logging it safe at all; `tankovault_config::explain` says so at the source.
///
/// The warning is the part worth having. A key supplied by two of the three *file-ish* layers
/// is refused at boot, but a mounted secret sitting under a stale `TANKOVAULT_*` variable is an
/// ordinary override the loader has no reason to refuse, and is exactly the shape of "the
/// rotated credential is not being picked up". Nothing here can fail a boot: a diagnostic that
/// takes the process down is worse than no diagnostic.
fn report_sources() {
    match tankovault_config::explain() {
        Ok(explanation) => {
            for origin in explanation.contested() {
                let shadowed = origin
                    .shadowed()
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ");
                tracing::warn!(
                    key = origin.key(),
                    effective = %origin.effective(),
                    %shadowed,
                    "a configuration key is supplied by more than one layer"
                );
            }
            tracing::debug!(layers = %explanation, "configuration sources");
        }
        Err(error) => tracing::debug!(%error, "the configuration layers could not be explained"),
    }
}

/// Runs a service, rebuilding it whenever its configuration files change.
///
/// `build` receives the current configuration and a token that is cancelled when the runtime
/// must stop, either because the process is shutting down or because a rebuild is due. It
/// should return once it has stopped: the replacement is not built until it does, so that the
/// old listener has released the address before the new one binds it.
///
/// Returns when the runtime returns of its own accord, which for a serving service means the
/// shutdown signal has been handled and in-flight requests have drained.
///
/// Generic over the runtime's error type so a service keeps whatever it already returns: every
/// one of them builds a pool or a client that fails with something other than a
/// [`ServiceError`](crate::ServiceError). The two `From` bounds are what let a watcher-install
/// failure and a failed re-read surface through it; `anyhow::Error`, which every service uses,
/// satisfies both by its blanket impl.
///
/// # Errors
/// Returns `E::from(WatchError)` if the filesystem watcher cannot be installed, or the runtime's
/// own error; a reload failure is logged and never returned.
pub async fn run<C, F, Fut, E>(
    boot: Loaded<C>,
    shutdown: &CancellationToken,
    build: F,
) -> Result<(), E>
where
    C: DeserializeOwned,
    F: Fn(Arc<C>, CancellationToken) -> Fut,
    Fut: Future<Output = Result<(), E>>,
    E: std::fmt::Display + From<WatchError> + From<ConfigError>,
{
    report_sources();

    terrace_config::reload::run(
        (boot.value, boot.sources),
        shutdown,
        || {
            tankovault_config::load_watched::<C>()
                .map(|loaded| (loaded.value, loaded.sources))
                .map_err(E::from)
        },
        build,
    )
    .await
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use secrecy::ExposeSecret as _;
    use terrace_config::testing::{Harness, Rebuilds};
    use tokio_util::sync::CancellationToken;

    /// How long the filesystem must be quiet before the supervisor acts, mirrored from
    /// `terrace_config::reload::Debounce`'s default so the waits below are sized against it.
    const DEBOUNCE: Duration = Duration::from_millis(500);

    #[derive(Debug, serde::Deserialize)]
    struct TestConfig {
        database: tankovault_config::DatabaseConfig,
    }

    /// A rotated secret rebuilds the runtime with the new value, and a reload that fails to load
    /// leaves the serving runtime alone.
    ///
    /// The second half is the one worth pinning: if a failed reload ever propagated instead of
    /// being swallowed, a single bad file write would take down every healthy pod at once, and
    /// nothing about the happy path would look different.
    ///
    /// Both halves are `terrace-config`'s behaviour, and it tests them. What is tested here is the
    /// wiring: this crate's reload closure re-reads through `tankovault_config::load_watched`, so
    /// a rotated `TANKOVAULT_SECRETS_DIR` entry is what the rebuilt runtime sees. A closure
    /// pointing anywhere else would still pass every upstream test, which is why the boot value
    /// is loaded through `tankovault_config` here, not through the jail's own loader.
    #[test]
    fn a_rotated_secret_rebuilds_and_a_broken_reload_does_not() {
        Harness::over(tankovault_config::terrace()).run(|jail| {
            let secrets = jail.secrets_dir()?;
            let url = jail.secret_key("database.url", "postgres://one/tv")?;

            let boot = tankovault_config::load_watched::<TestConfig>()?;
            let rebuilds: Rebuilds = Rebuilds::new();

            jail.block_on(async {
                let shutdown = CancellationToken::new();
                let driver = rebuilds.clone();
                let stop = shutdown.clone();

                tokio::spawn(async move {
                    driver.wait_for(1).await;
                    std::fs::write(&url, "postgres://two/tv").expect("rotate the mounted secret");
                    driver.wait_for(2).await;

                    // `.` is refused as a key, so this is a reload that fails to *load* while
                    // the directory it lives in is still perfectly readable.
                    std::fs::write(secrets.join("bad.key"), "x").expect("break the mount");
                    driver.stays_at(2, DEBOUNCE * 4).await;

                    stop.cancel();
                });

                super::run(
                    boot,
                    &shutdown,
                    rebuilds
                        .serving(|cfg: &TestConfig| cfg.database.url.expose_secret().to_owned()),
                )
                .await
                .expect("the supervisor returns when shutdown is cancelled");
            });

            assert_eq!(
                rebuilds.seen(),
                ["postgres://one/tv", "postgres://two/tv"],
                "the rebuild must use the rotated value, exactly once"
            );
            Ok(())
        });
    }
}
