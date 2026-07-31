//! Exponential-backoff-with-jitter retry decorator for transient errors.

use crate::error::FetchError;
use crate::fetcher::Fetcher;
use crate::types::{FetchRequest, FetchResponse};
use async_trait::async_trait;
use std::time::Duration;

/// Retries the inner fetcher on transient failures with capped exponential backoff.
pub struct RetryingFetcher<F> {
    inner: F,
    max_attempts: u32,
    base_delay: Duration,
    max_delay: Duration,
}

impl<F> RetryingFetcher<F> {
    /// `max_attempts` total tries (design caps this at ~4); backoff grows from
    /// `base_delay` up to `max_delay`, each step jittered.
    #[must_use]
    pub fn new(inner: F, max_attempts: u32, base_delay: Duration, max_delay: Duration) -> Self {
        Self {
            inner,
            max_attempts: max_attempts.max(1),
            base_delay,
            max_delay,
        }
    }
}

#[async_trait]
impl<F: Fetcher> Fetcher for RetryingFetcher<F> {
    async fn get(&self, req: FetchRequest) -> Result<FetchResponse, FetchError> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            match self.inner.get(req.clone()).await {
                Ok(resp) => return Ok(resp),
                Err(err) if err.is_transient() && attempt < self.max_attempts => {
                    let backoff = self.backoff(attempt);
                    tracing::debug!(attempt, ?backoff, error = %err, "retrying transient fetch error");
                    tokio::time::sleep(backoff).await;
                }
                Err(err) => return Err(err),
            }
        }
    }
}

impl<F> RetryingFetcher<F> {
    /// Jittered exponential backoff for `attempt` (1-based), capped at `max_delay`.
    ///
    /// One line because the policy is shared; see [`crate::jitter`].
    fn backoff(&self, attempt: u32) -> Duration {
        crate::jitter::full_jitter_now(self.base_delay, self.max_delay, attempt)
    }
}
