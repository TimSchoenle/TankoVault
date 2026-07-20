//! # tankovault-fetch
//!
//! The **only** place network egress to providers happens (design §9). A composition of
//! decorators over `reqwest`, outer → inner:
//!
//! ```text
//! RobotsFetcher      -> honours robots.txt + crawl-delay; refuses disallowed paths
//! RateLimitedFetcher -> per-provider governor + concurrency cap
//! SolvingFetcher     -> detects bot-management challenges; delegates to a ChallengeSolver
//! RetryingFetcher    -> exponential backoff + jitter on transient errors
//! BaseHttpFetcher    -> reqwest with a validating (SSRF-safe) DNS resolver + realistic headers
//! ```
//!
//! Hard invariant: **no image/content fetch path exists**. [`FetchResponse::body`] is
//! bounded UTF-8 text for HTML/JSON parsing only.

mod base;
mod builder;
mod error;
mod fetcher;
mod ratelimit;
mod retry;
mod robots;
mod solver_client;
mod solving;
pub mod ssrf;
mod types;

pub use base::BaseHttpFetcher;
pub use builder::{ProviderFetchConfig, build_provider_fetcher};
pub use error::FetchError;
pub use fetcher::Fetcher;
pub use ratelimit::RateLimitedFetcher;
pub use retry::RetryingFetcher;
pub use robots::{RobotsFetcher, RobotsRules};
pub use solver_client::HttpChallengeSolver;
pub use solving::{InMemorySessionStore, SessionStore, SolvedSession, SolvingFetcher};
pub use types::{FetchRequest, FetchResponse};
