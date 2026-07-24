//! Compose the provider fetch stack.
//!
//! The stack is built **per provider** (each carries its own politeness, robots rules,
//! and default UA), outer → inner:
//! `Robots → RateLimited → Solving → Retry → Base`.

use crate::base::BaseHttpFetcher;
use crate::error::FetchError;
use crate::fetcher::Fetcher;
use crate::ratelimit::RateLimitedFetcher;
use crate::retry::RetryingFetcher;
use crate::robots::{RobotsFetcher, RobotsRules};
use crate::solving::{SessionStore, SolvingFetcher};
use std::sync::Arc;
use std::time::Duration;
use tankovault_solver::ChallengeSolver;

/// Everything needed to build a provider's fetch stack.
pub struct ProviderFetchConfig {
    /// Default user-agent for ordinary requests.
    pub user_agent: String,
    /// Requests per second (already clamped to policy).
    pub rps: f64,
    /// Max concurrent requests.
    pub concurrency: u32,
    /// Crawl-delay floor between requests, ms.
    pub crawl_delay_ms: u64,
    /// Parsed robots rules, or `None` to skip robots enforcement.
    pub robots: Option<RobotsRules>,
    /// TCP connect timeout.
    pub connect_timeout: Duration,
    /// Whole-request timeout.
    pub request_timeout: Duration,
    /// Max total fetch attempts (retry cap).
    pub max_attempts: u32,
    /// The challenge solver back-end (HTTP client to the service, or a fake in tests).
    pub solver: Arc<dyn ChallengeSolver>,
    /// Solved-session cache.
    pub session_store: Arc<dyn SessionStore>,
}

impl ProviderFetchConfig {
    /// Sensible defaults for a provider, given its UA and solver/session wiring.
    #[must_use]
    pub fn new(
        user_agent: impl Into<String>,
        solver: Arc<dyn ChallengeSolver>,
        session_store: Arc<dyn SessionStore>,
    ) -> Self {
        Self {
            user_agent: user_agent.into(),
            rps: 1.0,
            concurrency: 2,
            crawl_delay_ms: 0,
            robots: None,
            connect_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(30),
            max_attempts: 4,
            solver,
            session_store,
        }
    }
}

/// Build the composed, provider-scoped fetch stack.
///
/// # Errors
/// Returns [`FetchError`] if the base HTTP client cannot be constructed.
pub fn build_provider_fetcher(cfg: ProviderFetchConfig) -> Result<Arc<dyn Fetcher>, FetchError> {
    let base = BaseHttpFetcher::new(cfg.user_agent, cfg.connect_timeout, cfg.request_timeout)?;
    let retry = RetryingFetcher::new(
        base,
        cfg.max_attempts,
        Duration::from_millis(500),
        Duration::from_secs(30),
    );
    let solving = SolvingFetcher::new(retry, cfg.solver, cfg.session_store);
    let rated = RateLimitedFetcher::new(solving, cfg.rps, cfg.concurrency, cfg.crawl_delay_ms);
    let robots = RobotsFetcher::new(rated, cfg.robots);
    Ok(Arc::new(robots))
}
