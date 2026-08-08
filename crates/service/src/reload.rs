//! Rebuilding a running service when the files its configuration came from change.
//!
//! The supervisor itself is [`terrace_config::reload`]: the watcher, the debounce, the no-op
//! detection and the failure posture all live there. What this module is, is the one wiring
//! decision that is ours — that the closure re-reading the configuration is
//! [`tankovault_config::load_watched`], so every service reloads through the same layers it
//! booted through.
//!
//! # What is *not* rebuilt
//! The process-global installations that happen before [`run`] is reached and cannot be redone:
//! the `tracing` subscriber ([`crate::init_tracing`]) and the metrics recorder
//! ([`crate::MetricsRegistry::install`]). Changing `telemetry.*` or `metrics.*` still needs a
//! restart.
//!
//! # Failure posture
//! A reload that cannot be loaded, or that fails to build, leaves the running service exactly
//! as it was. This matters more than it sounds: the reload path runs the same code that at boot
//! only ever ran under a scheduler that would restart the container, so a bad file write must
//! not be able to take down a pod that is currently healthy.

use std::future::Future;
use std::sync::Arc;

use serde::de::DeserializeOwned;
use tokio_util::sync::CancellationToken;

use tankovault_config::{ConfigError, Loaded};
use terrace_config::reload::WatchError;

/// Run a service, rebuilding it whenever its configuration files change.
///
/// `build` receives the current configuration and a token that is cancelled when the runtime
/// must stop — either because the process is shutting down or because a rebuild is due. It
/// should return once it has stopped: the replacement is not built until it does, so that the
/// old listener has released the address before the new one binds it.
///
/// Returns when the runtime returns of its own accord, which for a serving service means the
/// shutdown signal has been handled and in-flight requests have drained.
///
/// Generic over the runtime's error type so a service keeps whatever it already returns — every
/// one of them builds a pool or a client that fails with something other than a
/// [`ServiceError`](crate::ServiceError). The two `From` bounds are what let a watcher-install
/// failure and a failed re-read surface through it; `anyhow::Error`, which every service uses,
/// satisfies both by its blanket impl.
///
/// # Errors
/// Returns `E::from(WatchError)` if the filesystem watcher cannot be installed, or whatever the
/// runtime itself returned. A *reload* failure is never returned — it is logged and the running
/// configuration is kept.
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

    /// How long the filesystem must be quiet before the supervisor acts, mirrored from
    /// `terrace_config::reload::Debounce`'s default so the waits below are sized against it.
    const DEBOUNCE: Duration = Duration::from_millis(500);

    /// The two behaviours the supervisor exists for, in one run: a rotated secret rebuilds the
    /// runtime with the new value, and a reload that *fails to load* leaves the runtime that is
    /// already serving completely alone.
    ///
    /// The second half is the one worth pinning. The reload path runs the same code that at
    /// boot only ever ran under a scheduler that would restart the container, so if a failed
    /// reload ever propagated instead of being swallowed, a single bad file write would take
    /// down every healthy pod at once — and nothing about the happy path would look different.
    ///
    /// Both halves are `terrace-config`'s behaviour, and it tests them. What is tested *here*
    /// is the wiring: that this crate's reload closure re-reads through
    /// `tankovault_config::load_watched`, so a rotated `TANKOVAULT_SECRETS_DIR` entry is what
    /// the rebuilt runtime sees. A closure pointing at anything else would still pass every
    /// test upstream.
    #[test]
    #[expect(
        clippy::result_large_err,
        reason = "figment::Jail::expect_with fixes the closure's error type"
    )]
    fn a_rotated_secret_rebuilds_and_a_broken_reload_does_not() {
        use secrecy::ExposeSecret as _;
        use std::sync::{Arc, Mutex};

        #[derive(Debug, serde::Deserialize)]
        struct TestConfig {
            database: tankovault_config::DatabaseConfig,
        }

        /// Long enough that a missed wake-up fails rather than hangs CI; the watcher normally
        /// answers within `DEBOUNCE`.
        const PATIENCE: Duration = Duration::from_secs(10);

        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.create_dir("secrets")?;
            jail.create_file("secrets/database__url", "postgres://one/tv")?;
            let dir = jail.directory().join("secrets");
            jail.set_env("TANKOVAULT_SECRETS_DIR", dir.display());

            let boot = tankovault_config::load_watched::<TestConfig>()
                .map_err(|e| e.to_string())
                .unwrap();

            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");

            runtime.block_on(async move {
                let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
                let shutdown = tokio_util::sync::CancellationToken::new();

                let recorded = Arc::clone(&seen);
                let driver_seen = Arc::clone(&seen);
                let driver_shutdown = shutdown.clone();
                let driver_dir = dir.clone();

                tokio::spawn(async move {
                    let count = |n: usize| {
                        let seen = Arc::clone(&driver_seen);
                        async move {
                            let deadline = tokio::time::Instant::now() + PATIENCE;
                            while seen.lock().expect("not poisoned").len() < n {
                                assert!(
                                    tokio::time::Instant::now() < deadline,
                                    "the supervisor never reached {n} builds"
                                );
                                tokio::time::sleep(Duration::from_millis(25)).await;
                            }
                        }
                    };

                    count(1).await;
                    std::fs::write(driver_dir.join("database__url"), "postgres://two/tv")
                        .expect("rotate");
                    count(2).await;

                    // `.` is refused as a key, so this is a reload that fails to *load* while
                    // the directory it lives in is still perfectly readable.
                    std::fs::write(driver_dir.join("bad.key"), "x").expect("break");
                    tokio::time::sleep(DEBOUNCE * 4).await;
                    assert_eq!(
                        driver_seen.lock().expect("not poisoned").len(),
                        2,
                        "a failed reload must not rebuild the running service"
                    );

                    driver_shutdown.cancel();
                });

                super::run(boot, &shutdown, move |cfg, token| {
                    recorded
                        .lock()
                        .expect("not poisoned")
                        .push(cfg.database.url.expose_secret().to_owned());
                    async move {
                        token.cancelled().await;
                        Ok::<(), anyhow::Error>(())
                    }
                })
                .await
                .expect("the supervisor returns when shutdown is cancelled");

                let seen = seen.lock().expect("not poisoned").clone();
                assert_eq!(
                    seen,
                    ["postgres://one/tv", "postgres://two/tv"],
                    "the rebuild must use the rotated value, exactly once"
                );
            });

            Ok(())
        });
    }
}
