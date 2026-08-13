//! Core solver types and the pluggable [`ChallengeSolver`] contract.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// The class of bot-management challenge detected on a response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChallengeKind {
    /// Classic Cloudflare "Just a moment…" JS challenge (`/cdn-cgi/challenge`).
    CloudflareJs,
    /// Cloudflare managed challenge (`cf-mitigated: challenge`).
    CloudflareManaged,
    /// Cloudflare Turnstile widget.
    Turnstile,
    /// A generic JS interstitial from a non-Cloudflare WAF.
    GenericJsInterstitial,
}

impl ChallengeKind {
    /// Stable metric/label token.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CloudflareJs => "cloudflare_js",
            Self::CloudflareManaged => "cloudflare_managed",
            Self::Turnstile => "turnstile",
            Self::GenericJsInterstitial => "generic_js_interstitial",
        }
    }
}

/// Renders the stable label token, so a challenge kind can be interpolated into an error
/// message or a log field without every call site reaching for `as_str`.
impl std::fmt::Display for ChallengeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A request to solve a challenge for a specific target URL.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SolveRequest {
    /// The absolute URL that returned a challenge.
    pub url: String,
    /// The provider slug (for per-provider rate limiting/metrics).
    pub provider: String,
    /// The detected challenge kind, if the caller classified it.
    pub kind: Option<ChallengeKind>,
}

/// A solved, reusable session plus optionally the solved HTML.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SolveOutcome {
    /// Cookies to replay on subsequent requests (e.g. `cf_clearance`, `__cf_bm`).
    pub cookies: Vec<(String, String)>,
    /// The user-agent the solver used — must match the cookies to stay valid.
    pub user_agent: String,
    /// The solved page HTML, when the solver fetched it (avoids a re-fetch).
    pub html: Option<String>,
    /// The HTTP status the solver's browser received for [`Self::html`], when the back-end
    /// reports one.
    ///
    /// Without this the fetch stack has to assume `200`, and every status the provider
    /// expresses through a *rendered page* — a `429` throttle notice, a `503` maintenance
    /// screen — reaches the caller as a successful fetch of an unparseable document. The
    /// layers that exist to handle those statuses (server-directed backoff, adaptive rate
    /// limiting) then never see them.
    #[serde(default)]
    pub status: Option<u16>,
    /// Response headers the solver observed, when the back-end reports them (`Retry-After`
    /// is the one that matters).
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    /// Seconds the session is expected to remain valid (drives Redis TTL).
    pub ttl_secs: u64,
}

/// Failure modes of a solve attempt.
#[derive(Debug, thiserror::Error)]
pub enum SolveError {
    /// Transport failure talking to the solver back-end.
    #[error("solver transport error: {0}")]
    Transport(String),
    /// The back-end ran but could not defeat the challenge.
    ///
    /// A statement about the *provider*: it is challenging us and the challenge held. Reserve it
    /// for that — see [`Self::Unavailable`] for the failure that looks identical from a call site
    /// and means the opposite.
    #[error("solver could not bypass the challenge: {0}")]
    Unsolved(String),
    /// The solver tier itself is not able to serve solves right now — the service is down, its
    /// browser pool is exhausted, or it is shedding load.
    ///
    /// Distinct from [`Self::Unsolved`] because the two lead to opposite conclusions and used to
    /// be reported identically. A challenge that cannot be beaten is the provider's answer, and
    /// re-asking is pointless; a solver that is not there says nothing about the provider at all,
    /// and the same request will very likely succeed once it is back. Collapsing them made a
    /// solver outage read, in the operator's failure feed, as nine providers having turned
    /// hostile — and, because it is not transient, cost every affected fetch its retries.
    #[error("solver back-end unavailable: {0}")]
    Unavailable(String),
    /// The back-end exceeded its time budget.
    #[error("solver timed out")]
    Timeout,
    /// The back-end response could not be parsed.
    #[error("malformed solver response: {0}")]
    Malformed(String),
}

impl SolveError {
    /// Whether another attempt could plausibly succeed without anything changing at the provider.
    ///
    /// Only failures of *our* tier qualify. [`Self::Unsolved`] does not: the back-end ran, the
    /// provider answered, and repeating that exchange re-runs an expensive browser solve for the
    /// same verdict. [`Self::Malformed`] does not either — a back-end whose response cannot be
    /// decoded is a broken contract, which a retry reproduces exactly.
    #[must_use]
    pub const fn is_transient(&self) -> bool {
        matches!(
            self,
            Self::Transport(_) | Self::Unavailable(_) | Self::Timeout
        )
    }
}

/// The pluggable bypass contract. Implementations: [`super::TrawlSolver`]
/// (default), a headless-render back-end (the `render` service), or a custom solver.
///
/// Keeping every back-end behind this one method is what makes the tier modular: the
/// fetch pipeline, adapters, and workers never name a concrete solver (design §9 note).
#[async_trait]
pub trait ChallengeSolver: Send + Sync {
    /// Solve the challenge for `req.url`, returning a reusable session.
    async fn solve(&self, req: SolveRequest) -> Result<SolveOutcome, SolveError>;

    /// A short identifier for the active back-end (surfaced in the console).
    fn backend_name(&self) -> &'static str;
}
