//! Cooperative shutdown.
//!
//! Every service previously ran `axum::serve(..).await` with no shutdown handling at all,
//! so a container stop killed in-flight requests and background loops mid-write. A single
//! [`CancellationToken`] fans one OS signal out to the HTTP server *and* to every spawned
//! loop (schedulers, consumers, sweeps), letting each finish its current unit of work.

use tokio_util::sync::CancellationToken;

/// Install the OS-signal listener and return the token it cancels.
///
/// Listens for `SIGINT` (Ctrl-C) everywhere and `SIGTERM` on Unix, which is what a
/// container runtime sends first on stop. The listener task ends after the first signal;
/// a second signal reaches the default handler and terminates the process immediately,
/// which is the conventional escape hatch when a drain hangs.
///
/// Clone the returned token freely — cancellation is observed by every clone.
#[must_use]
pub fn install_shutdown() -> CancellationToken {
    let token = CancellationToken::new();
    let listener = token.clone();
    tokio::spawn(async move {
        let reason = wait_for_signal().await;
        tracing::info!(signal = reason, "shutdown signal received; draining");
        listener.cancel();
    });
    token
}

/// Resolve on the first termination signal, naming which one arrived.
async fn wait_for_signal() -> &'static str {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        // A failure to register a handler is not recoverable and must not silently
        // downgrade to "never shuts down", so fall back to Ctrl-C only.
        let mut terminate = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = %e, "cannot listen for SIGTERM; SIGINT only");
                let _ = tokio::signal::ctrl_c().await;
                return "SIGINT";
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => "SIGINT",
            _ = terminate.recv() => "SIGTERM",
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        "SIGINT"
    }
}

/// Run `task` on `interval`, stopping promptly when `shutdown` is cancelled.
///
/// The shared shape of every background loop in the system (scheduler sweeps, reconcile
/// passes, retention sweeps). Written once here so none of them re-implement the
/// select-on-cancel dance — the previous hand-rolled loops had no cancellation at all and
/// could be killed part-way through a database write.
///
/// The first tick fires immediately after `interval` elapses, not at start-up, so a
/// rolling restart does not stampede every replica's sweep at once.
pub async fn every<F, Fut>(
    interval: std::time::Duration,
    shutdown: CancellationToken,
    name: &'static str,
    mut task: F,
) where
    F: FnMut() -> Fut + Send,
    Fut: Future<Output = ()> + Send,
{
    let mut ticker = tokio::time::interval(interval);
    // `interval` yields immediately on its first tick; consume it so the delay applies
    // before the first real run.
    ticker.tick().await;
    loop {
        tokio::select! {
            () = shutdown.cancelled() => {
                tracing::debug!(loop_name = name, "background loop stopping");
                return;
            }
            _ = ticker.tick() => task().await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    #[tokio::test(start_paused = true)]
    async fn every_runs_until_cancelled() {
        let runs = Arc::new(AtomicU32::new(0));
        let token = CancellationToken::new();

        let counter = Arc::clone(&runs);
        let loop_token = token.clone();
        let handle = tokio::spawn(async move {
            every(Duration::from_secs(10), loop_token, "test", move || {
                let counter = Arc::clone(&counter);
                async move {
                    counter.fetch_add(1, Ordering::Relaxed);
                }
            })
            .await;
        });

        // The first tick is consumed at start-up, so nothing has run yet. With paused
        // time, `sleep` auto-advances once every task is idle, which also gives the
        // spawned loop a chance to be polled — `advance` alone does not.
        tokio::time::sleep(Duration::from_secs(5)).await;
        assert_eq!(runs.load(Ordering::Relaxed), 0);

        tokio::time::sleep(Duration::from_secs(25)).await;
        let observed = runs.load(Ordering::Relaxed);
        assert!(observed >= 2, "expected repeated runs, saw {observed}");

        token.cancel();
        handle.await.expect("loop task should exit cleanly");
    }

    #[tokio::test]
    async fn cancellation_is_observed_by_clones() {
        let token = CancellationToken::new();
        let clone = token.clone();
        token.cancel();
        assert!(clone.is_cancelled());
    }
}
