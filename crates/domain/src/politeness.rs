//! Per-provider crawl politeness: rate, concurrency, delay, and the identity presented.
//!
//! Operators may tune these downward only. The ceilings in this module bound how aggressively
//! any configuration can crawl a provider.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Hard upper bound on requests-per-second for any single provider, per worker process.
///
/// A worker executes one scan task at a time, so this ceiling, not the task loop, bounds
/// catalogue-scan throughput. A provider sees `rps × replicas`, so this is the per-process half
/// of a budget that has to be divided by the replica count.
pub const MAX_RPS: f64 = 8.0;
/// Hard **lower** bound on requests-per-second: one request every 1000 seconds.
///
/// The consumer turns `rps` into a period, and a non-positive value's reciprocal overflows
/// `Duration::from_secs_f64`, which panics. Anything below this is indistinguishable from "off",
/// which is what `active` is for.
pub const MIN_RPS: f64 = 0.001;
/// Hard upper bound on concurrent in-flight requests for any single provider, per worker
/// process.
///
/// Paired with [`MAX_RPS`]: in-flight requests are what let a crawl reach its `rps` when
/// provider latency is high, so lifting one without the other leaves the rate unreachable.
pub const MAX_CONCURRENCY: u32 = 16;
/// Default identifiable crawler user-agent.
///
/// Only sent when `emulation` is `None`; an emulated client must send the user-agent that
/// belongs to its TLS/HTTP2 fingerprint (see [`BrowserEmulation`]).
pub const DEFAULT_USER_AGENT: &str =
    "TankoVaultBot/0.1 (+https://github.com/tankovault; metadata-aggregator; contact: operator)";

/// Which browser the fetch stack impersonates at the TLS/HTTP2 layer.
///
/// Providers sit behind Cloudflare or DDoS-Guard, which fingerprint the TLS `ClientHello` and the
/// HTTP/2 SETTINGS frame and compare them against the `User-Agent` header. A mismatch is a
/// stronger signal than an unknown client, so the whole profile is picked as one unit from a
/// browser family: cipher suites, extension order, ALPS, header order and casing, and the
/// user-agent itself.
///
/// Families, not versions: the concrete build resolves to whatever the emulation catalogue
/// considers newest, so bumping the catalogue needs no database migration or API-schema change.
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
    /// `OkHttp`, the Android HTTP client, for providers whose mobile API is friendlier than
    /// their web front end.
    OkHttp,
}

/// Crawl politeness parameters for one provider.
///
/// `rps` and `concurrency` are enforced per worker process, not fleet-wide. The fetch stack is
/// built per provider inside each worker, so a provider sees `rps × replicas`. Size a budget by
/// dividing the intended aggregate by the replica count; scaling workers out without lowering
/// these raises the real crawl rate.
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
    /// User-agent sent on ordinary, non-challenge requests, used only when `emulation` is
    /// `None`. An emulated client sends its profile's user-agent, because a custom string over a
    /// browser TLS fingerprint is the mismatch WAFs look for.
    #[serde(default = "Politeness::default_user_agent")]
    pub user_agent: String,
    /// Browser to impersonate, or `None` to crawl as an identifiable bot using `user_agent`.
    #[serde(default = "Politeness::default_emulation")]
    pub emulation: Option<BrowserEmulation>,
}

impl Politeness {
    // The default is what an unconfigured provider crawls at, so it is a policy choice, not a
    // placeholder, sized against the ceilings above.
    fn default_rps() -> f64 {
        2.0
    }
    fn default_concurrency() -> u32 {
        4
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

    /// Clamps all tunables into policy.
    ///
    /// `crates/fetch` takes these numbers without re-validating them, so the result must be
    /// usable whatever was configured. A non-finite `rps` is replaced, not clamped: `NaN`
    /// satisfies neither `>` nor `<`, and `f64::clamp` returns `NaN` unchanged.
    #[must_use]
    pub fn clamped(mut self) -> Self {
        self.rps = if self.rps.is_finite() {
            self.rps.clamp(MIN_RPS, MAX_RPS)
        } else {
            Self::default_rps()
        };
        self.concurrency = self.concurrency.clamp(1, MAX_CONCURRENCY);
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

/// The politeness block as a request body carries it.
///
/// Mirrors [`Politeness`] field for field and differs in one way: an absent `emulation` means no
/// emulation, where [`Politeness`] reads an absent key as [`BrowserEmulation::Chrome`].
///
/// Both readings are needed, so the shapes cannot be one type. [`Politeness`] is also the stored
/// JSONB document, and a stored row without the key must keep crawling under emulation, not as
/// an identifiable bot. A client generated from this API's schema cannot send an explicit `null`,
/// since `Option` fields are omitted when `None`. If absent meant Chrome here too, choosing "no
/// emulation" in the console would silently keep Chrome and report the save as successful.
///
/// The exhaustive `From` below fails to compile on a new stored field, and
/// `every_stored_field_survives_the_request_shape` catches one hardcoded to silence it.
#[derive(Debug, Deserialize, ToSchema)]
pub struct PolitenessInput {
    /// Requests per second, per worker process (see [`Politeness`] on fleet totals).
    #[serde(default = "Politeness::default_rps")]
    pub rps: f64,
    /// Maximum concurrent requests to this provider, per worker process.
    #[serde(default = "Politeness::default_concurrency")]
    pub concurrency: u32,
    /// Minimum delay between requests, in milliseconds.
    #[serde(default)]
    pub crawl_delay_ms: u64,
    /// User-agent sent on ordinary, non-challenge requests, used only when `emulation` is absent.
    #[serde(default = "Politeness::default_user_agent")]
    pub user_agent: String,
    /// Browser to impersonate. Absent (or null) crawls as an identifiable bot using `user_agent`.
    #[serde(default)]
    pub emulation: Option<BrowserEmulation>,
}

impl Default for PolitenessInput {
    fn default() -> Self {
        // Not `Politeness::default()`: this must agree with what an empty `{}` body deserializes
        // to, and that carries no `emulation` key, which here means no emulation. A request that
        // omits the block entirely is a different statement, which the handler answers with the
        // server's defaults.
        Self {
            rps: Politeness::default_rps(),
            concurrency: Politeness::default_concurrency(),
            crawl_delay_ms: 0,
            user_agent: Politeness::default_user_agent(),
            emulation: None,
        }
    }
}

impl From<PolitenessInput> for Politeness {
    fn from(input: PolitenessInput) -> Self {
        Self {
            rps: input.rps,
            concurrency: input.concurrency,
            crawl_delay_ms: input.crawl_delay_ms,
            user_agent: input.user_agent,
            emulation: input.emulation,
        }
    }
}

#[cfg(test)]
mod tests {
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
        assert_eq!(p.rps, MIN_RPS);
        assert_eq!(p.concurrency, 1);
    }

    /// A clamped rate survives being turned into a period. A rate clamped to
    /// `f64::MIN_POSITIVE` would overflow `Duration::from_secs_f64` and panic the worker at
    /// fetcher construction, and `NaN` would skip the guard, since it satisfies neither `>` nor
    /// `<=`.
    #[test]
    fn a_clamped_rate_survives_conversion_to_a_period() {
        for rps in [
            0.0,
            -1.0,
            f64::MIN_POSITIVE,
            1e-300,
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            100.0,
        ] {
            let clamped = Politeness {
                rps,
                concurrency: 1,
                crawl_delay_ms: 0,
                user_agent: "x".into(),
                emulation: None,
            }
            .clamped()
            .rps;
            assert!(
                (MIN_RPS..=MAX_RPS).contains(&clamped),
                "rps {rps} clamped to {clamped}, outside [{MIN_RPS}, {MAX_RPS}]"
            );
            // Precisely what `tankovault_fetch::RateLimitedFetcher::new` does with it.
            let _period = std::time::Duration::from_secs_f64(1.0 / clamped);
        }
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

    /// Providers stored before emulation existed deserialize into the new default,
    /// not into "no emulation", so an upgrade does not silently keep crawling as a bot.
    /// This is the half of the contract [`PolitenessInput`] deliberately inverts.
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

    /// A request that names no emulation asks for none, the opposite of what the same absence
    /// means in a stored document. `Default` has to agree with an empty body, since a caller
    /// cannot tell which of the two a server used to fill the gaps.
    #[test]
    fn a_request_without_emulation_means_no_emulation() {
        let sparse: PolitenessInput = serde_json::from_str(r#"{"rps":1.0}"#).unwrap();
        assert!(sparse.emulation.is_none());
        assert!(PolitenessInput::default().emulation.is_none());
        let explicit: PolitenessInput = serde_json::from_str(r#"{"emulation":null}"#).unwrap();
        assert!(explicit.emulation.is_none());
    }

    /// `PolitenessInput` mirrors `Politeness` by hand, and a tunable carried by one and not the
    /// other is one the API publishes and then throws away. A new `Politeness` field fails to
    /// compile in the `From` impl; this catches the field filled with a constant instead of read
    /// off the request, so every value below is non-default.
    #[test]
    fn every_stored_field_survives_the_request_shape() {
        let tuned = Politeness {
            rps: 3.5,
            concurrency: 7,
            crawl_delay_ms: 250,
            user_agent: "TankoVault/test".to_owned(),
            emulation: Some(BrowserEmulation::Firefox),
        };
        let sent = serde_json::to_value(&tuned).expect("the stored shape serialises");
        let received: PolitenessInput =
            serde_json::from_value(sent).expect("the request shape carries every stored field");
        assert_eq!(Politeness::from(received), tuned);
    }
}
