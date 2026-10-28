//! Defines [`FetchError`].

use crate::ssrf::SsrfError;
use tankovault_solver::ChallengeKind;

/// A fetch failure; [`Self::is_transient`] decides whether [`crate::RetryingFetcher`]
/// retries.
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
    /// The solver tier could not serve the solve: the service is down, its browser pool is
    /// saturated, or it is shedding load.
    ///
    /// Separate from [`Self::Solver`] because this says nothing about the provider: the request
    /// never reached a browser. Merged, a solver outage would read as every gated provider
    /// turning hostile at once.
    #[error("solver unavailable: {0}")]
    SolverUnavailable(String),
    /// The request timed out.
    #[error("request timed out")]
    Timeout,
    /// The redirect cap was exceeded.
    #[error("too many redirects")]
    TooManyRedirects,
    /// The response body exceeded the size cap, which guards memory and the no-content
    /// invariant.
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
    /// Returns whether a retry could succeed: timeouts, transport blips, 5xx.
    ///
    /// Scoped to what [`crate::RetryingFetcher`] acts on, the innermost layer below the solver.
    /// [`Self::SolverUnavailable`] is absent although transient: it is raised above that layer,
    /// and [`crate::RetryingSolver`] retries it in the stack, `AdapterError::is_transient` across
    /// task deliveries.
    #[must_use]
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            Self::Timeout | Self::Transport(_) | Self::Status(500..=599)
        )
    }
}
