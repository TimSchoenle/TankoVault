//! # tankovault-fetch
//!
//! The **only** place network egress to providers happens (design §9): `wreq` wrapped in
//! backoff, rate-limiting, challenge-solving and retry decorators, outer to inner.
//!
//! Hard invariant: **no image/content fetch path exists**. [`FetchResponse::body`] is
//! bounded UTF-8 text for HTML/JSON parsing only.

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
