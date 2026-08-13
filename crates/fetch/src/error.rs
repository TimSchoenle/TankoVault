//! Fetch stack error type.

use crate::ssrf::SsrfError;
use tankovault_solver::ChallengeKind;

/// A fetch failure. `is_transient` decides whether [`crate::RetryingFetcher`] retries.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    /// Blocked by the SSRF guard.
    #[error("ssrf guard: {0}")]
    Ssrf(#[from] SsrfError),
    /// A non-success HTTP status the caller treats as an error.
    #[error("http status {0}")]
    Status(u16),
    /// A challenge was detected and could not be solved within budget.
    #[error("unsolved challenge: {0:?}")]
    Challenge(ChallengeKind),
    /// The solver ran and the challenge held.
    #[error("solver error: {0}")]
    Solver(String),
    /// The solver tier could not serve the solve at all — the service is down, its browser pool
    /// is saturated, or it is shedding load.
    ///
    /// Separate from [`Self::Solver`] because the two say opposite things about the provider.
    /// This one says nothing about it: the request never reached a browser. Reported as one
    /// failure, a solver outage read as every gated provider having turned hostile at once.
    #[error("solver unavailable: {0}")]
    SolverUnavailable(String),
    /// The request timed out.
    #[error("request timed out")]
    Timeout,
    /// The redirect cap was exceeded.
    #[error("too many redirects")]
    TooManyRedirects,
    /// The response body exceeded the size cap (guards memory + the no-content invariant).
    #[error("response body exceeded the size cap")]
    BodyTooLarge,
    /// A transport-level failure.
    #[error("transport error: {0}")]
    Transport(String),
    /// The URL could not be parsed.
    #[error("invalid url: {0}")]
    InvalidUrl(String),
}

impl FetchError {
    /// Whether a retry could plausibly succeed (timeouts, transport blips, 5xx).
    ///
    /// Scoped to what [`crate::RetryingFetcher`] can act on, which is the innermost layer — below
    /// the solver. [`Self::SolverUnavailable`] is therefore deliberately absent even though it is
    /// transient in the ordinary sense: it is raised *above* the retrying layer, and repeating it
    /// is [`crate::RetryingSolver`]'s job in-stack and `AdapterError::is_transient`'s across task
    /// deliveries. Listing it here would read as a policy nothing implements.
    #[must_use]
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            Self::Timeout | Self::Transport(_) | Self::Status(500..=599)
        )
    }
}
