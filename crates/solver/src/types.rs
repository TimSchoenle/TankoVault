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
    #[error("solver could not bypass the challenge: {0}")]
    Unsolved(String),
    /// The back-end exceeded its time budget.
    #[error("solver timed out")]
    Timeout,
    /// The back-end response could not be parsed.
    #[error("malformed solver response: {0}")]
    Malformed(String),
}

/// The pluggable bypass contract. Implementations: [`super::FlareSolverrSolver`]
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
