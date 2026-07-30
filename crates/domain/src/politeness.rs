//! Per-provider crawl politeness: request rate, concurrency, crawl delay, and the browser
//! identity presented to the provider.
//!
//! Operators may tune these **downward** (more polite) but a set of hard ceilings
//! bound them so no configuration can crawl a provider more aggressively than the
//! system permits (design §9 "operator-tunable downward … bounded by hard ceilings").

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Hard upper bound on requests-per-second for any single provider, per worker process.
pub const MAX_RPS: f64 = 4.0;
/// Hard upper bound on concurrent in-flight requests for any single provider, per worker
/// process.
pub const MAX_CONCURRENCY: u32 = 8;
/// Default identifiable crawler user-agent.
///
/// Only sent when `emulation` is `None`; an emulated client must send the user-agent that
/// belongs to its TLS/HTTP2 fingerprint (see [`BrowserEmulation`]).
pub const DEFAULT_USER_AGENT: &str =
    "TankoVaultBot/0.1 (+https://github.com/tankovault; metadata-aggregator; contact: operator)";

/// Which browser the fetch stack impersonates at the TLS/HTTP2 layer.
///
/// Providers sit behind Cloudflare/DDoS-Guard, which fingerprint the TLS `ClientHello` and
/// the HTTP/2 SETTINGS frame and compare them against the `User-Agent` header. A mismatch
/// is a stronger signal than an unknown client, so the whole profile — cipher suites,
/// extension order, ALPS, header order and casing, and the user-agent itself — is picked as
/// one unit from a browser family rather than assembled field by field.
///
/// Families, not versions: the concrete build is resolved to whatever the emulation
/// catalogue currently considers newest, so bumping the catalogue does not require a
/// database migration or an API-schema change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BrowserEmulation {
    /// Chrome on Windows. The default: the largest share of real traffic, so the least
    /// remarkable fingerprint to present.
    Chrome,
    /// Firefox on Windows.
    Firefox,
    /// Safari on macOS.
    Safari,
    /// Edge on Windows.
    Edge,
    /// `OkHttp` — the Android HTTP client. For providers whose mobile API is friendlier
    /// than their web front end.
    OkHttp,
}

/// Crawl politeness parameters for one provider.
///
/// **`rps` and `concurrency` are enforced per worker process, not fleet-wide.** The fetch
/// stack is built per provider inside each worker, so the load a provider actually sees is
/// `rps × replicas`. Sizing a provider's budget means dividing the intended aggregate by the
/// replica count; scaling workers out without lowering these raises the real crawl rate.
/// (A cross-replica token bucket in Redis is the documented follow-up — see
/// `crates/fetch/src/ratelimit.rs`.)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Politeness {
    /// Requests per second, per worker process (see the type-level note on fleet totals).
    #[serde(default = "Politeness::default_rps")]
    pub rps: f64,
    /// Maximum concurrent requests to this provider, per worker process.
    #[serde(default = "Politeness::default_concurrency")]
    pub concurrency: u32,
    /// Minimum delay between requests, in milliseconds.
    #[serde(default)]
    pub crawl_delay_ms: u64,
    /// User-agent sent on ordinary (non-challenge) requests — **only when `emulation` is
    /// `None`**. An emulated client sends the user-agent belonging to its profile, because
    /// a custom string over a browser TLS fingerprint is exactly the mismatch WAFs look for.
    #[serde(default = "Politeness::default_user_agent")]
    pub user_agent: String,
    /// Browser to impersonate, or `None` to crawl as an identifiable bot using `user_agent`.
    #[serde(default = "Politeness::default_emulation")]
    pub emulation: Option<BrowserEmulation>,
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
    // Always `Some`, but the signature must match the field's type for `#[serde(default)]`.
    #[expect(
        clippy::unnecessary_wraps,
        reason = "must match the Option<BrowserEmulation> field it defaults"
    )]
    fn default_emulation() -> Option<BrowserEmulation> {
        Some(BrowserEmulation::Chrome)
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
            emulation: Self::default_emulation(),
        }
    }
}

#[cfg(test)]
mod tests {
    // Clamping returns exactly the ceiling constants, so exact float comparison is correct.
    #![expect(
        clippy::float_cmp,
        reason = "these assertions compare clamped values against the exact bounds they were \
                  clamped to, so equality is the property under test"
    )]

    use super::*;

    #[test]
    fn clamps_above_ceiling() {
        let p = Politeness {
            rps: 100.0,
            concurrency: 999,
            crawl_delay_ms: 0,
            user_agent: "x".into(),
            emulation: None,
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
            emulation: None,
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

    #[test]
    fn default_emulates_chrome() {
        assert_eq!(
            Politeness::default().emulation,
            Some(BrowserEmulation::Chrome)
        );
    }

    /// Providers stored before emulation existed deserialize into the new default rather
    /// than into "no emulation", so an upgrade does not silently keep crawling as a bot.
    #[test]
    fn missing_emulation_deserializes_to_default() {
        let p: Politeness = serde_json::from_str(r#"{"rps":1.0,"concurrency":2}"#).unwrap();
        assert_eq!(p.emulation, Some(BrowserEmulation::Chrome));
    }

    #[test]
    fn explicit_null_emulation_disables_it() {
        let p: Politeness = serde_json::from_str(r#"{"emulation":null}"#).unwrap();
        assert!(p.emulation.is_none());
    }
}
