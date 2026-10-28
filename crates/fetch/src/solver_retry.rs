//! Retries a solve that failed because the solver tier was not there.
//!
//! A decorator over [`ChallengeSolver`], not a branch in [`crate::solving`], because the failure
//! is not the crawl's: a saturated browser pool or a restarting service is this deployment's own
//! transient, whichever back-end is mounted. Every back-end (the HTTP client, TRAWL in-process, a
//! fake) inherits the policy unchanged.
//!
//! Narrow on purpose. Only [`SolveError::is_transient`] is retried, so a challenge that held is
//! never re-solved: a browser solve costs seconds and returns the same verdict. The budget is
//! small because this runs inside the provider's concurrency permit, which no other request for
//! that provider can use meanwhile.

use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;
use tankovault_solver::{ChallengeSolver, SolveError, SolveOutcome, SolveRequest};

/// Wraps a solver, repeating a solve the tier could not serve.
pub struct RetryingSolver {
    inner: Arc<dyn ChallengeSolver>,
    max_attempts: u32,
    base_delay: Duration,
    max_delay: Duration,
}

impl RetryingSolver {
    /// Wraps `inner` with up to `max_attempts` tries (1 disables retrying), waiting from
    /// `base_delay` up to `max_delay` with jitter.
    #[must_use]
    pub fn new(
        inner: Arc<dyn ChallengeSolver>,
        max_attempts: u32,
        base_delay: Duration,
        max_delay: Duration,
    ) -> Self {
        Self {
            inner,
            max_attempts: max_attempts.max(1),
            base_delay,
            max_delay,
        }
    }

    /// Wraps `inner` with the default budget: three tries over at most a few seconds.
    ///
    /// Sized for a browser slot coming free or a solver replica finishing a restart, at the cost
    /// of one idle provider permit. Longer outages are the task retry ladder's business: minutes,
    /// with no permit held.
    #[must_use]
    pub fn with_default_budget(inner: Arc<dyn ChallengeSolver>) -> Self {
        Self::new(inner, 3, Duration::from_millis(500), Duration::from_secs(4))
    }
}

#[async_trait]
impl ChallengeSolver for RetryingSolver {
    async fn solve(&self, req: SolveRequest) -> Result<SolveOutcome, SolveError> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            match self.inner.solve(req.clone()).await {
                Ok(outcome) => return Ok(outcome),
                Err(err) if err.is_transient() && attempt < self.max_attempts => {
                    let wait =
                        crate::jitter::full_jitter_now(self.base_delay, self.max_delay, attempt);
                    tracing::debug!(
                        provider = %req.provider,
                        attempt,
                        ?wait,
                        error = %err,
                        "solver tier unavailable; retrying the solve"
                    );
                    metrics::counter!(
                        "solve_retries_total",
                        "provider" => req.provider.clone(),
                    )
                    .increment(1);
                    tokio::time::sleep(wait).await;
                }
                Err(err) => return Err(err),
            }
        }
    }

    fn backend_name(&self) -> &'static str {
        self.inner.backend_name()
    }
}

#[cfg(test)]
mod tests {
    use super::RetryingSolver;
    use async_trait::async_trait;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;
    use tankovault_solver::{ChallengeSolver, SolveError, SolveOutcome, SolveRequest};

    /// Fails `failures` times with `error`, then succeeds, counting every call.
    struct FlakySolver {
        calls: AtomicU32,
        failures: u32,
        error: fn(String) -> SolveError,
    }

    impl FlakySolver {
        fn new(failures: u32, error: fn(String) -> SolveError) -> Arc<Self> {
            Arc::new(Self {
                calls: AtomicU32::new(0),
                failures,
                error,
            })
        }
    }

    #[async_trait]
    impl ChallengeSolver for FlakySolver {
        async fn solve(&self, _req: SolveRequest) -> Result<SolveOutcome, SolveError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n < self.failures {
                return Err((self.error)("nope".to_owned()));
            }
            Ok(SolveOutcome {
                cookies: Vec::new(),
                user_agent: "UA/1.0".to_owned(),
                html: Some("<html>ok</html>".to_owned()),
                status: Some(200),
                headers: Vec::new(),
                ttl_secs: 600,
            })
        }

        fn backend_name(&self) -> &'static str {
            "flaky"
        }
    }

    fn request() -> SolveRequest {
        SolveRequest {
            url: "https://provider.example/manga/x".to_owned(),
            provider: "nyxscans".to_owned(),
            kind: None,
        }
    }

    fn retrying(inner: Arc<dyn ChallengeSolver>) -> RetryingSolver {
        // Near-zero waits: this tests which errors are repeated and how often; the delay is
        // `crate::jitter`'s.
        RetryingSolver::new(inner, 3, Duration::from_millis(1), Duration::from_millis(2))
    }

    /// An unavailable tier is tried again.
    ///
    /// The bug: with nothing repeating the solve, a briefly unavailable solver tier reports
    /// "solver could not bypass the challenge" for providers answering `200` to a plain request.
    #[tokio::test]
    async fn an_unavailable_tier_is_tried_again() {
        let inner = FlakySolver::new(2, SolveError::Unavailable);
        retrying(inner.clone())
            .solve(request())
            .await
            .expect("the third attempt succeeds");
        assert_eq!(inner.calls.load(Ordering::SeqCst), 3);
    }

    /// A challenge that held is not re-solved: it is the provider's answer, and repeating it
    /// re-runs a full browser solve for the same verdict.
    #[tokio::test]
    async fn a_challenge_that_held_is_not_re_solved() {
        let inner = FlakySolver::new(1, SolveError::Unsolved);
        let err = retrying(inner.clone())
            .solve(request())
            .await
            .expect_err("an unsolved challenge is returned as-is");
        assert!(matches!(err, SolveError::Unsolved(_)), "got {err:?}");
        assert_eq!(inner.calls.load(Ordering::SeqCst), 1);
    }

    /// The budget is finite: a tier that stays down surfaces the failure instead of holding the
    /// provider's concurrency permit indefinitely.
    #[tokio::test]
    async fn the_attempt_budget_is_finite() {
        let inner = FlakySolver::new(u32::MAX, SolveError::Unavailable);
        let err = retrying(inner.clone())
            .solve(request())
            .await
            .expect_err("the budget runs out");
        assert!(matches!(err, SolveError::Unavailable(_)), "got {err:?}");
        assert_eq!(inner.calls.load(Ordering::SeqCst), 3);
    }

    /// The decorator keeps the back-end's name: `backend_name` is what the console reports as
    /// the active solver.
    #[test]
    fn the_wrapped_back_end_keeps_its_name() {
        let inner = FlakySolver::new(0, SolveError::Unavailable);
        assert_eq!(retrying(inner).backend_name(), "flaky");
    }
}
