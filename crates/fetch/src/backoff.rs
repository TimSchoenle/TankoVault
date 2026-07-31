//! Server-directed backoff: honour a provider telling us to slow down.
//!
//! The rate limiter enforces the crawl budget *we* chose; this layer enforces the one the
//! **provider** asks for. On a `429 Too Many Requests` or `503 Service Unavailable` that
//! survived challenge solving, it waits — preferring the server's own `Retry-After` over
//! our guess — and retries, instead of spending the rest of the crawl hammering a host that
//! has explicitly asked us to stop. At catalogue scale (tens of thousands of requests per
//! run) this is the difference between a crawl that degrades politely and one that earns a
//! block.
//!
//! **Placement matters.** This sits *outside* [`crate::SolvingFetcher`], not inside the
//! inner [`crate::RetryingFetcher`]: 429 and 503 are also two of the three statuses
//! Cloudflare serves interstitials with (`detect_challenge`'s `CHALLENGE_STATUSES`), so a
//! layer below the solver could not tell "slow down" from "solve this" and would burn every
//! attempt retrying a challenge the solver was about to handle. By the time a response
//! reaches this layer the solver has already had its turn, so a remaining 429/503 is a
//! genuine rate-limit or outage signal.
//!
//! It also sits *outside* [`crate::RateLimitedFetcher`], so each retry re-acquires a rate
//! token and a concurrency permit rather than slipping past the crawl budget.

use crate::error::FetchError;
use crate::fetcher::Fetcher;
use crate::types::{FetchRequest, FetchResponse};
use async_trait::async_trait;
use std::time::Duration;

/// Statuses that mean "you are going too fast" or "come back later".
///
/// Deliberately excludes `403`: a bare forbidden is an authorization/blocking verdict that
/// retrying cannot change, and it is the primary challenge status.
const BACKOFF_STATUSES: [u16; 2] = [429, 503];

/// Waits and retries when a provider signals rate-limit or unavailability.
pub struct BackoffFetcher<F> {
    inner: F,
    max_attempts: u32,
    base_delay: Duration,
    max_delay: Duration,
}

impl<F> BackoffFetcher<F> {
    /// `max_attempts` total tries. Waits come from `Retry-After` when the provider sends a
    /// usable one, otherwise from jittered exponential backoff growing from `base_delay`.
    /// Every wait is capped at `max_delay` so one hostile or mistaken header cannot park a
    /// worker for hours.
    #[must_use]
    pub fn new(inner: F, max_attempts: u32, base_delay: Duration, max_delay: Duration) -> Self {
        Self {
            inner,
            max_attempts: max_attempts.max(1),
            base_delay,
            max_delay,
        }
    }

    /// Jittered exponential backoff for `attempt` (1-based), capped at `max_delay`.
    ///
    /// The policy itself lives in [`crate::jitter`], shared with [`crate::retry`] — the two
    /// layers differ in *what* they retry, not in how long they wait, and this was the same
    /// eleven lines in both files.
    fn backoff(&self, attempt: u32) -> Duration {
        crate::jitter::full_jitter_now(self.base_delay, self.max_delay, attempt)
    }

    /// How long to wait before the next attempt, preferring the server's instruction.
    fn wait_for(&self, resp: &FetchResponse, attempt: u32) -> Duration {
        retry_after(resp).map_or_else(
            || self.backoff(attempt),
            |requested| requested.min(self.max_delay),
        )
    }
}

/// Parse a `Retry-After` delta-seconds value.
///
/// Only the numeric form is honoured. The HTTP-date form is legal but rare in practice, and
/// acting on a misparsed absolute date is worse than falling back to our own backoff — so an
/// unparseable value simply yields `None`.
fn retry_after(resp: &FetchResponse) -> Option<Duration> {
    let raw = resp.header("retry-after")?.trim();
    let secs: u64 = raw.parse().ok()?;
    Some(Duration::from_secs(secs))
}

#[async_trait]
impl<F: Fetcher> Fetcher for BackoffFetcher<F> {
    async fn get(&self, req: FetchRequest) -> Result<FetchResponse, FetchError> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            let resp = self.inner.get(req.clone()).await?;
            if !BACKOFF_STATUSES.contains(&resp.status) || attempt >= self.max_attempts {
                return Ok(resp);
            }
            let wait = self.wait_for(&resp, attempt);
            tracing::warn!(
                provider = %req.provider_slug,
                status = resp.status,
                attempt,
                wait_ms = wait.as_millis(),
                server_directed = retry_after(&resp).is_some(),
                "provider asked us to slow down; backing off"
            );
            tokio::time::sleep(wait).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Serves a scripted sequence of statuses, recording how many calls it saw.
    struct Scripted {
        statuses: Vec<u16>,
        retry_after: Option<&'static str>,
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Fetcher for Scripted {
        async fn get(&self, req: FetchRequest) -> Result<FetchResponse, FetchError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            let status = *self
                .statuses
                .get(n)
                .unwrap_or(self.statuses.last().unwrap());
            let headers = match self.retry_after {
                Some(v) if status == 429 || status == 503 => {
                    vec![("Retry-After".to_owned(), v.to_owned())]
                }
                _ => Vec::new(),
            };
            Ok(FetchResponse {
                status,
                url: req.url.clone(),
                headers,
                body: String::new(),
                from_cache: false,
            })
        }
    }

    fn req() -> FetchRequest {
        FetchRequest::new("https://example.test/a", "example")
    }

    fn fetcher(
        statuses: Vec<u16>,
        retry_after: Option<&'static str>,
    ) -> (BackoffFetcher<Scripted>, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let inner = Scripted {
            statuses,
            retry_after,
            calls: calls.clone(),
        };
        let f = BackoffFetcher::new(
            inner,
            4,
            Duration::from_millis(1),
            Duration::from_millis(20),
        );
        (f, calls)
    }

    #[tokio::test]
    async fn retries_rate_limit_until_success() {
        let (f, calls) = fetcher(vec![429, 429, 200], None);
        let resp = f.get(req()).await.expect("succeeds");
        assert_eq!(resp.status, 200);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn retries_service_unavailable() {
        let (f, calls) = fetcher(vec![503, 200], None);
        assert_eq!(f.get(req()).await.expect("succeeds").status, 200);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn gives_up_after_max_attempts_and_returns_the_last_response() {
        // The caller still sees the 429 and can fail the task; we do not retry forever.
        let (f, calls) = fetcher(vec![429], None);
        assert_eq!(f.get(req()).await.expect("returns response").status, 429);
        assert_eq!(calls.load(Ordering::SeqCst), 4);
    }

    #[tokio::test]
    async fn a_forbidden_is_not_retried() {
        // 403 is the solver's business, not ours — retrying it wastes the crawl budget.
        let (f, calls) = fetcher(vec![403], None);
        assert_eq!(f.get(req()).await.expect("returns response").status, 403);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn success_passes_straight_through() {
        let (f, calls) = fetcher(vec![200], None);
        assert_eq!(f.get(req()).await.expect("succeeds").status, 200);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn an_absurd_retry_after_is_capped_not_obeyed() {
        // A one-hour Retry-After must not park the worker: the wait is clamped to max_delay.
        let (f, _) = fetcher(vec![429, 200], Some("3600"));
        let started = std::time::Instant::now();
        assert_eq!(f.get(req()).await.expect("succeeds").status, 200);
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "waited {:?}; the cap was not applied",
            started.elapsed()
        );
    }

    #[test]
    fn parses_only_the_numeric_retry_after_form() {
        let with = |v: &str| FetchResponse {
            status: 429,
            url: String::new(),
            headers: vec![("Retry-After".to_owned(), v.to_owned())],
            body: String::new(),
            from_cache: false,
        };
        assert_eq!(retry_after(&with("120")), Some(Duration::from_secs(120)));
        assert_eq!(retry_after(&with("  30 ")), Some(Duration::from_secs(30)));
        // HTTP-date form is not honoured; we fall back to our own backoff.
        assert_eq!(retry_after(&with("Wed, 21 Oct 2026 07:28:00 GMT")), None);
        assert_eq!(retry_after(&with("nonsense")), None);
    }
}
