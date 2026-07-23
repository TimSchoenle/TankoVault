//! Per-provider crawl politeness: request rate, concurrency, crawl delay, user-agent.
//!
//! Operators may tune these **downward** (more polite) but a set of hard ceilings
//! bound them so no configuration can crawl a provider more aggressively than the
//! system permits (design §9 "operator-tunable downward … bounded by hard ceilings").

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Hard upper bound on requests-per-second for any single provider.
pub const MAX_RPS: f64 = 4.0;
/// Hard upper bound on concurrent in-flight requests for any single provider.
pub const MAX_CONCURRENCY: u32 = 8;
/// Default identifiable crawler user-agent.
pub const DEFAULT_USER_AGENT: &str =
    "TankoVaultBot/0.1 (+https://github.com/tankovault; metadata-aggregator; contact: operator)";

/// Crawl politeness parameters for one provider.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Politeness {
    /// Requests per second, aggregate across the worker pool.
    #[serde(default = "Politeness::default_rps")]
    pub rps: f64,
    /// Maximum concurrent requests to this provider.
    #[serde(default = "Politeness::default_concurrency")]
    pub concurrency: u32,
    /// Minimum delay between requests, in milliseconds (from robots crawl-delay or config).
    #[serde(default)]
    pub crawl_delay_ms: u64,
    /// User-agent sent on ordinary (non-challenge) requests.
    #[serde(default = "Politeness::default_user_agent")]
    pub user_agent: String,
}

impl Politeness {
    fn default_rps() -> f64 {
        1.0
    }
    fn default_concurrency() -> u32 {
        2
    }
    fn default_user_agent() -> String {
        DEFAULT_USER_AGENT.to_owned()
    }

    /// Clamp all tunables to the hard ceilings. Returns a value guaranteed to be
    /// within policy regardless of what was configured.
    #[must_use]
    pub fn clamped(mut self) -> Self {
        if self.rps > MAX_RPS || self.rps <= 0.0 {
            self.rps = self.rps.clamp(f64::MIN_POSITIVE, MAX_RPS);
        }
        if self.concurrency == 0 || self.concurrency > MAX_CONCURRENCY {
            self.concurrency = self.concurrency.clamp(1, MAX_CONCURRENCY);
        }
        self
    }
}

impl Default for Politeness {
    fn default() -> Self {
        Self {
            rps: Self::default_rps(),
            concurrency: Self::default_concurrency(),
            crawl_delay_ms: 0,
            user_agent: Self::default_user_agent(),
        }
    }
}

#[cfg(test)]
mod tests {
    // Clamping returns exactly the ceiling constants, so exact float comparison is correct.
    #![allow(clippy::float_cmp)]

    use super::*;

    #[test]
    fn clamps_above_ceiling() {
        let p = Politeness {
            rps: 100.0,
            concurrency: 999,
            crawl_delay_ms: 0,
            user_agent: "x".into(),
        }
        .clamped();
        assert_eq!(p.rps, MAX_RPS);
        assert_eq!(p.concurrency, MAX_CONCURRENCY);
    }

    #[test]
    fn clamps_zero_or_negative() {
        let p = Politeness {
            rps: 0.0,
            concurrency: 0,
            crawl_delay_ms: 0,
            user_agent: "x".into(),
        }
        .clamped();
        assert!(p.rps > 0.0);
        assert_eq!(p.concurrency, 1);
    }

    #[test]
    fn default_is_polite() {
        let p = Politeness::default();
        assert!(p.rps <= MAX_RPS);
        assert!(p.concurrency <= MAX_CONCURRENCY);
    }
}
