//! Rebuilding a running service when the files its configuration came from change.
//!
//! A Kubernetes `Secret` or `ConfigMap` mounted as a volume is updated in place by the kubelet:
//! a new timestamped directory is written and `..data` is renamed over the old one. That is the
//! only way a long-lived process learns a credential was rotated — environment variables are
//! fixed for the life of a process — so a service that takes its secrets from files can pick up
//! a rotation without being restarted.
//!
//! [`run`] takes the closure that builds the whole runtime and re-runs it. Everything the
//! closure builds is rebuilt: the connection pool, the application state, the router, the
//! listener, the background tasks. That is deliberate — the alternative, hot-swapping
//! individual fields behind shared handles, means every consumer has to be correct against a
//! value that changes underneath it, and the failure when one is not is a service running half
//! on the old configuration.
//!
//! # What is *not* rebuilt
//! The process-global installations that happen before [`run`] is reached and cannot be redone:
//! the `tracing` subscriber ([`crate::init_tracing`]) and the metrics recorder
//! ([`crate::MetricsRegistry::install`]). Changing `telemetry.*` or `metrics.*` still needs a
//! restart, and [`run`] says so in the log rather than pretending otherwise.
//!
//! # Failure posture
//! A reload that cannot be loaded, or that fails to build, leaves the running service exactly
//! as it was. This matters more than it sounds: the reload path runs the same code that at boot
//! only ever ran under a scheduler that would restart the container, so a bad file write must
//! not be able to take down a pod that is currently healthy.

use std::future::Future;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde::de::DeserializeOwned;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use tankovault_config::{Loaded, Sources};

use crate::ServiceError;

/// How long the filesystem must be quiet before a change is acted on.
///
/// One logical Kubernetes volume update fires several events — the new directory is created,
/// files are written, `..data` is renamed — and rebuilding on the first of them would read a
/// half-written mount. The kubelet's own sync period is on the order of a minute, so half a
/// second of extra latency costs nothing and removes the whole class of torn reads.
const DEBOUNCE: Duration = Duration::from_millis(500);

/// The channel depth between the watcher thread and the supervisor.
///
/// One, because the signal carries no information: any pending notification means "re-read",
/// and a burst of fifty is the same instruction as one. A full channel is therefore dropped
/// rather than queued.
const SIGNAL_DEPTH: usize = 1;

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
/// Generic over the runtime's error type so a service keeps whatever it already returns —
/// every one of them builds a pool or a client that fails with something other than a
/// [`ServiceError`]. The `From<ServiceError>` bound is what lets a watcher-install failure
/// surface through it.
///
/// # Errors
/// Returns [`ServiceError::Watch`] if the filesystem watcher cannot be installed, or whatever
/// the runtime itself returned. A *reload* failure is never returned — it is logged and the
/// running configuration is kept.
pub async fn run<C, F, Fut, E>(
    boot: Loaded<C>,
    shutdown: &CancellationToken,
    build: F,
) -> Result<(), E>
where
    C: DeserializeOwned,
    F: Fn(Arc<C>, CancellationToken) -> Fut,
    Fut: Future<Output = Result<(), E>>,
    E: std::fmt::Display + From<ServiceError>,
{
    let mut sources = boot.sources;
    let mut config = Arc::new(boot.value);
    let mut changes = Watch::install(sources.watch_paths()).map_err(E::from)?;

    let mut generation = shutdown.child_token();
    let running = build(Arc::clone(&config), generation.clone());
    tokio::pin!(running);

    loop {
        tokio::select! {
            outcome = &mut running => return outcome,
            () = changes.changed() => {
                let Some(next) = reload::<C>(&sources) else { continue };

                tracing::info!("configuration changed; rebuilding the service");
                generation.cancel();
                // Driven to completion rather than dropped: dropping the future would sever
                // in-flight requests and leave the listener's address in TIME_WAIT while the
                // replacement tries to bind it.
                if let Err(e) = (&mut running).await {
                    tracing::warn!(error = %e, "the previous runtime stopped with an error");
                }

                sources = next.sources;
                config = Arc::new(next.value);
                generation = shutdown.child_token();
                running.set(build(Arc::clone(&config), generation.clone()));
            }
        }
    }
}

/// Re-read the configuration, returning it only if it resolves to different values.
///
/// `None` covers both "nothing actually changed" and "the reload failed": neither is a reason
/// to touch a service that is currently working, and both are the common case — a `..data` swap
/// that moved no key, or a half-written mount caught between events.
fn reload<C: DeserializeOwned>(current: &Sources) -> Option<Loaded<C>> {
    match tankovault_config::load_watched::<C>() {
        Err(e) => {
            tracing::error!(
                error = %e,
                "configuration reload failed; keeping the running configuration"
            );
            None
        }
        Ok(next) if !next.sources.differs_from(current) => {
            tracing::debug!("configuration files changed but resolved to the same values");
            None
        }
        Ok(next) => Some(next),
    }
}

/// The filesystem watcher and the signal it feeds.
///
/// Holds the `notify` watcher itself: dropping it stops the watch, so it has to outlive the
/// supervisor loop rather than the call that installed it.
struct Watch {
    /// `None` when there is nothing to watch — no secrets directory, no `_FILE` indirection —
    /// in which case [`Self::changed`] never resolves and the service simply runs.
    signals: Option<mpsc::Receiver<()>>,
    /// Kept alive for its `Drop`; never read.
    _watcher: Option<notify::RecommendedWatcher>,
}

impl Watch {
    /// Install a watch over every configured directory.
    ///
    /// # Errors
    /// Returns [`ServiceError::Watch`] if the platform watcher cannot be created or a directory
    /// cannot be watched.
    fn install(paths: &[std::path::PathBuf]) -> Result<Self, ServiceError> {
        let watchable: Vec<&Path> = paths
            .iter()
            .map(std::path::PathBuf::as_path)
            // A path that does not exist cannot be watched, and is not an error: a service with
            // no secrets directory is the normal development case.
            .filter(|p| p.is_dir())
            .collect();
        if watchable.is_empty() {
            tracing::debug!("no configuration directories to watch; reload is inactive");
            return Ok(Self {
                signals: None,
                _watcher: None,
            });
        }

        let (tx, rx) = mpsc::channel(SIGNAL_DEPTH);
        let mut watcher =
            notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                // The callback runs on `notify`'s own thread. `try_send` rather than a blocking
                // send: a full channel already means "re-read pending", so dropping this one loses
                // nothing and never parks the watcher thread.
                if event.is_ok() {
                    let _ = tx.try_send(());
                }
            })
            .map_err(|e| ServiceError::Watch(e.to_string()))?;

        for path in watchable {
            // Non-recursive: a `Secret` volume is flat, and recursing into the timestamped
            // `..data` target would double every event for no extra coverage.
            notify::Watcher::watch(&mut watcher, path, notify::RecursiveMode::NonRecursive)
                .map_err(|e| ServiceError::Watch(format!("watching {}: {e}", path.display())))?;
            tracing::debug!(path = %path.display(), "watching for configuration changes");
        }

        Ok(Self {
            signals: Some(rx),
            _watcher: Some(watcher),
        })
    }

    /// Resolve once the watched files have changed *and* gone quiet again.
    async fn changed(&mut self) {
        let Some(signals) = self.signals.as_mut() else {
            // Nothing to watch: never resolve, so the caller's `select!` reduces to just
            // running the service.
            std::future::pending::<()>().await;
            return;
        };

        if signals.recv().await.is_none() {
            // The watcher is gone (its thread died). Reload is over; the service keeps running.
            self.signals = None;
            std::future::pending::<()>().await;
            return;
        }

        tokio::time::sleep(DEBOUNCE).await;
        // Collapse the rest of the burst so one logical update rebuilds once.
        while signals.try_recv().is_ok() {}
    }
}

#[cfg(test)]
mod tests {
    use super::{DEBOUNCE, Watch};
    use std::time::Duration;

    /// A watch over a directory fires on a file written into it, and collapses the burst that
    /// one write produces into a single wake-up.
    #[tokio::test]
    async fn a_write_into_a_watched_directory_wakes_the_supervisor() {
        let dir = std::env::temp_dir().join(format!("tv-reload-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let mut watch = Watch::install(std::slice::from_ref(&dir)).expect("watch installs");

        std::fs::write(dir.join("auth__jwt_secret"), "rotated").expect("write");

        tokio::time::timeout(DEBOUNCE + Duration::from_secs(5), watch.changed())
            .await
            .expect("the write must wake the watcher");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// With nothing to watch, `changed()` must never resolve — otherwise the supervisor's
    /// `select!` would spin, rebuilding the service as fast as it can construct it.
    #[tokio::test]
    async fn nothing_to_watch_never_wakes() {
        let mut watch = Watch::install(&[]).expect("an empty watch is not an error");
        let woke = tokio::time::timeout(Duration::from_millis(200), watch.changed()).await;
        assert!(woke.is_err(), "an empty watch must never resolve");
    }

    /// The two behaviours the supervisor exists for, in one run: a rotated secret rebuilds the
    /// runtime with the new value, and a reload that *fails to load* leaves the runtime that is
    /// already serving completely alone.
    ///
    /// The second half is the one worth pinning. The reload path runs the same code that at
    /// boot only ever ran under a scheduler that would restart the container, so if a failed
    /// reload ever propagated instead of being swallowed, a single bad file write would take
    /// down every healthy pod at once — and nothing about the happy path would look different.
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
                        Ok::<(), crate::ServiceError>(())
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
