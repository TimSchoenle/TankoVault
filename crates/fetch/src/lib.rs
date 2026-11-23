//! Owns the **only** network egress to providers (`docs/ARCHITECTURE.md` §9): `wreq` wrapped in
//! backoff, rate-limiting, challenge-solving and retry decorators, outer to inner.
//!
//! Hard invariant: **no image or content fetch path exists**. [`FetchResponse::body`] is UTF-8
//! text for HTML and JSON parsing only, capped at 16 MiB on both the direct and the solver path.

pub mod accounting;
mod backoff;
mod base;
mod builder;
mod error;
mod fetcher;
mod identity;
mod jitter;
mod ratelimit;
mod retry;
mod solver_client;
mod solver_retry;
mod solving;
pub mod ssrf;
mod types;

pub use accounting::{FetchAccounting, Metered, measured};
pub use backoff::BackoffFetcher;
pub use base::BaseHttpFetcher;
pub use builder::{ProviderFetchConfig, build_provider_fetcher};
pub use error::FetchError;
pub use fetcher::Fetcher;
pub use ratelimit::{RateLimitedFetcher, ThrottlePolicy};
pub use retry::RetryingFetcher;
pub use solver_client::HttpChallengeSolver;
pub use solver_retry::RetryingSolver;
pub use solving::{InMemorySessionStore, SessionStore, SolvedSession, SolvingFetcher};
pub use types::{FetchRequest, FetchResponse};

/// Seams for the out-of-workspace fuzz crate, which can only reach `pub` items.
#[doc(hidden)]
pub mod __fuzz {
    /// Runs the multi-member gzip decoder the direct fetch applies to a `gzip` body.
    ///
    /// # Errors
    /// Returns whatever error `decode_gzip` returns.
    pub fn decode_gzip(raw: &[u8]) -> Result<Vec<u8>, crate::FetchError> {
        crate::base::decode_gzip(raw)
    }
}
