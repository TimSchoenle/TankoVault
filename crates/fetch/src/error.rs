//! Fetch stack error type.

use crate::ssrf::SsrfError;
use tankovault_solver::ChallengeKind;

/// A fetch failure. `is_transient` decides whether [`crate::RetryingFetcher`] retries.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    /// Blocked by the SSRF guard.
    #[error("ssrf guard: {0}")]
    Ssrf(#[from] SsrfError),
    /// The path is disallowed by the provider's robots.txt.
    #[error("path disallowed by robots.txt: {0}")]
    RobotsDisallowed(String),
    /// A non-success HTTP status the caller treats as an error.
    #[error("http status {0}")]
    Status(u16),
    /// A challenge was detected and could not be solved within budget.
    #[error("unsolved challenge: {0:?}")]
    Challenge(ChallengeKind),
    /// The solver back-end errored.
    #[error("solver error: {0}")]
    Solver(String),
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
    #[must_use]
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            Self::Timeout | Self::Transport(_) | Self::Status(500..=599)
        )
    }
}
