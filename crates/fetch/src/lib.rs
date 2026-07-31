//! # tankovault-fetch
//!
//! The **only** place network egress to providers happens (design §9). A composition of
//! decorators over `wreq`, outer → inner:
//!
//! ```text
//! BackoffFetcher     -> honours provider-directed 429/503 + Retry-After
//! RateLimitedFetcher -> per-provider governor + concurrency cap + crawl delay
//! SolvingFetcher     -> detects bot-management challenges; delegates to a ChallengeSolver
//! RetryingFetcher    -> exponential backoff + jitter on transient errors
//! BaseHttpFetcher    -> wreq with browser emulation + a validating (SSRF-safe) DNS resolver
//! ```
//!
//! Hard invariant: **no image/content fetch path exists**. [`FetchResponse::body`] is
//! bounded UTF-8 text for HTML/JSON parsing only.

mod backoff;
mod base;
mod builder;
mod error;
mod fetcher;
mod jitter;
mod ratelimit;
mod retry;
mod solver_client;
mod solving;
pub mod ssrf;
mod types;

pub use backoff::BackoffFetcher;
pub use base::BaseHttpFetcher;
pub use builder::{ProviderFetchConfig, build_provider_fetcher};
pub use error::FetchError;
pub use fetcher::Fetcher;
pub use ratelimit::{RateLimitedFetcher, ThrottlePolicy};
pub use retry::RetryingFetcher;
pub use solver_client::HttpChallengeSolver;
pub use solving::{InMemorySessionStore, SessionStore, SolvedSession, SolvingFetcher};
pub use types::{FetchRequest, FetchResponse};
