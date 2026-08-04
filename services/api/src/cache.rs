//! A read-through snapshot cache for read models too expensive to recompute per request.
//!
//! The operator console's two rollups aggregate the whole catalogue — `count(*)` over `chapters`
//! and a group-by of every chapter row against its provider — which is a full scan of the largest
//! table in the database each time. Measured at 5.7–7.5 s while a scan was running, and paid
//! twice over, because the console's stats tab and its providers tab both fetch the per-provider
//! table.
//!
//! [`Cached`] is *stale-while-revalidate*: a snapshot older than the TTL is still returned, and
//! the refresh it triggers runs behind the response. That is deliberate, and it is why the
//! numbers stay exact counts rather than becoming `reltuples` estimates — the expensive query is
//! taken off the request path instead of being made cheaper and wrong. The cost is that a
//! displayed figure can lag by a TTL plus one query, which for an operator dashboard is not a
//! cost at all.
//!
//! Only one refresh runs at a time. Without that, the two console tabs loading together would
//! start two full scans, and a dashboard left open in three browser tabs would keep the database
//! permanently busy aggregating the same rows.

use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock};

/// How long a console rollup is served before a refresh is started behind the response.
pub const ADMIN_STATS_TTL: Duration = Duration::from_secs(30);

/// A value and when it was computed.
struct Snapshot<T> {
    value: T,
    taken_at: Instant,
}

/// A single cached value, refreshed on demand rather than on a timer.
///
/// On a timer it would aggregate the catalogue all night for nobody. Demand-driven, a console
/// nobody has open costs nothing.
pub struct Cached<T> {
    current: RwLock<Option<Snapshot<T>>>,
    /// Held for the duration of a refresh, so only one is ever in flight.
    refreshing: Mutex<()>,
    /// `None` disables caching outright — see [`Cached::uncached`].
    ttl: Option<Duration>,
}

impl<T: Clone + Send + Sync + 'static> Cached<T> {
    /// A cache holding nothing, whose first read computes the value.
    #[must_use]
    pub fn new(ttl: Duration) -> Arc<Self> {
        Arc::new(Self {
            current: RwLock::new(None),
            refreshing: Mutex::new(()),
            ttl: Some(ttl),
        })
    }

    /// A pass-through that computes the value on every read.
    ///
    /// For test harnesses. A zero TTL would *not* do: a stale snapshot is still served, so a
    /// test writing a row and reading the rollup back would see the numbers from before its
    /// write and be right to call that a bug.
    #[must_use]
    pub fn uncached() -> Arc<Self> {
        Arc::new(Self {
            current: RwLock::new(None),
            refreshing: Mutex::new(()),
            ttl: None,
        })
    }

    /// The cached value, computing it with `load` if there is nothing to serve yet.
    ///
    /// A stale value is returned as-is and refreshed behind the response; only the very first
    /// call (and the first after a failure) waits for `load`.
    ///
    /// # Errors
    /// Whatever `load` returns, and only when there is no snapshot at all — once one exists, a
    /// failing refresh is logged and the previous snapshot keeps being served.
    pub async fn get<F, Fut, E>(self: &Arc<Self>, load: F) -> Result<T, E>
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<T, E>> + Send + 'static,
        E: std::fmt::Display + Send + 'static,
    {
        let Some(ttl) = self.ttl else {
            return load().await;
        };
        let held = {
            let current = self.current.read().await;
            current
                .as_ref()
                .map(|snapshot| (snapshot.value.clone(), snapshot.taken_at.elapsed() >= ttl))
        };
        match held {
            Some((value, stale)) => {
                if stale {
                    self.refresh_behind_the_response(load);
                }
                Ok(value)
            }
            None => self.refresh_now(load).await,
        }
    }

    /// Compute the value inline, letting concurrent callers share the one computation.
    async fn refresh_now<F, Fut, E>(self: &Arc<Self>, load: F) -> Result<T, E>
    where
        F: Fn() -> Fut,
        Fut: Future<Output = Result<T, E>>,
    {
        let _guard = self.refreshing.lock().await;
        // Whoever held the lock may have filled the cache while this caller waited for it.
        if let Some(snapshot) = self.current.read().await.as_ref() {
            return Ok(snapshot.value.clone());
        }
        let value = load().await?;
        self.store(value.clone()).await;
        Ok(value)
    }

    /// Start a refresh that outlives this request, unless one is already running.
    fn refresh_behind_the_response<F, Fut, E>(self: &Arc<Self>, load: F)
    where
        F: Fn() -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, E>> + Send + 'static,
        E: std::fmt::Display + Send + 'static,
    {
        let cache = Arc::clone(self);
        tokio::spawn(async move {
            // Not `lock()`: a queued refresh would recompute what the running one is about to
            // store. Whoever holds it is already doing this caller's work.
            let Ok(_guard) = cache.refreshing.try_lock() else {
                return;
            };
            match load().await {
                Ok(value) => cache.store(value).await,
                Err(error) => tracing::warn!(
                    %error,
                    "refreshing a cached read model failed; serving the previous snapshot"
                ),
            }
        });
    }

    async fn store(&self, value: T) {
        *self.current.write().await = Some(Snapshot {
            value,
            taken_at: Instant::now(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A loader counting how often it ran, so the tests assert on work done rather than timing.
    ///
    /// Takes the counter by value: the closure is spawned, so it has to outlive the caller.
    fn counting(
        calls: Arc<AtomicUsize>,
    ) -> impl Fn() -> std::future::Ready<Result<usize, String>> + Send + Sync + 'static {
        move || {
            let seen = calls.fetch_add(1, Ordering::SeqCst) + 1;
            std::future::ready(Ok(seen))
        }
    }

    /// Give the spawned refresh a chance to run. Bounded rather than a single `yield_now` so the
    /// tests do not depend on how many polls the scheduler needs to drain one task.
    async fn settle() {
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test]
    async fn a_fresh_snapshot_is_served_without_recomputing_it() {
        let calls = Arc::new(AtomicUsize::new(0));
        let cache = Cached::new(Duration::from_secs(60));

        assert_eq!(cache.get(counting(Arc::clone(&calls))).await, Ok(1));
        assert_eq!(cache.get(counting(Arc::clone(&calls))).await, Ok(1));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "one computation, two reads"
        );
    }

    /// A stale read must answer from the snapshot it already has — the whole point is that the
    /// expensive query never blocks a response once one has succeeded.
    #[tokio::test]
    async fn a_stale_snapshot_is_served_while_the_refresh_runs_behind_it() {
        let calls = Arc::new(AtomicUsize::new(0));
        let cache = Cached::new(Duration::ZERO);

        assert_eq!(cache.get(counting(Arc::clone(&calls))).await, Ok(1));
        assert_eq!(
            cache.get(counting(Arc::clone(&calls))).await,
            Ok(1),
            "the stale value is returned, not the one being computed"
        );
        // Let the spawned refresh finish, then observe that it did.
        settle().await;
        assert_eq!(cache.get(counting(Arc::clone(&calls))).await, Ok(2));
    }

    /// A failing loader must not poison a cache that already holds something.
    #[tokio::test]
    async fn a_failed_refresh_keeps_serving_the_previous_snapshot() {
        let cache = Cached::new(Duration::ZERO);

        assert_eq!(
            cache.get(|| std::future::ready(Ok::<_, String>(7))).await,
            Ok(7)
        );
        assert_eq!(
            cache
                .get(|| std::future::ready(Err::<i32, _>("database is down".to_owned())))
                .await,
            Ok(7),
            "a stale hit never waits for the loader, so its failure is invisible here"
        );
        settle().await;
        assert_eq!(
            cache
                .get(|| std::future::ready(Err::<i32, _>("still down".to_owned())))
                .await,
            Ok(7),
            "and the failed refresh left the snapshot in place"
        );
    }

    /// The harness mode has to be a genuine pass-through, or every integration test reading a
    /// rollup back after a write is reading the snapshot from before it.
    #[tokio::test]
    async fn the_uncached_mode_recomputes_every_time() {
        let calls = Arc::new(AtomicUsize::new(0));
        let cache = Cached::uncached();

        assert_eq!(cache.get(counting(Arc::clone(&calls))).await, Ok(1));
        assert_eq!(cache.get(counting(Arc::clone(&calls))).await, Ok(2));
        assert_eq!(cache.get(counting(Arc::clone(&calls))).await, Ok(3));
    }

    /// With nothing cached, an error is the caller's to handle.
    #[tokio::test]
    async fn the_first_call_surfaces_the_loader_error() {
        let cache: Arc<Cached<i32>> = Cached::new(Duration::from_secs(60));

        assert_eq!(
            cache
                .get(|| std::future::ready(Err::<i32, _>("database is down".to_owned())))
                .await,
            Err("database is down".to_owned())
        );
    }
}
