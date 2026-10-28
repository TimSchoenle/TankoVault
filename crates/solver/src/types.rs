//! Defines the solver types and the pluggable [`ChallengeSolver`] contract.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// The class of bot-management challenge detected on a response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChallengeKind {
    /// The Cloudflare "Just a moment…" JS challenge (`/cdn-cgi/challenge`).
    CloudflareJs,
    /// The Cloudflare managed challenge (`cf-mitigated: challenge`).
    CloudflareManaged,
    /// The Cloudflare Turnstile widget.
    Turnstile,
    /// A generic JS interstitial from a non-Cloudflare WAF.
    GenericJsInterstitial,
}

impl ChallengeKind {
    /// Returns the stable metric label token.
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
    /// The provider slug, for per-provider rate limiting and metrics.
    pub provider: String,
    /// The detected challenge kind, if the caller classified it.
    pub kind: Option<ChallengeKind>,
}

/// A solved, reusable session, optionally with the solved HTML.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SolveOutcome {
    /// The cookies to replay on later requests, such as `cf_clearance` and `__cf_bm`.
    pub cookies: Vec<(String, String)>,
    /// The user agent the solver used; the cookies stay valid only with it.
    pub user_agent: String,
    /// The solved page HTML, when the solver fetched it, which saves a re-fetch.
    pub html: Option<String>,
    /// The HTTP status the solver's browser received for [`Self::html`], when the back-end
    /// reports one.
    ///
    /// Without it the fetch stack has to assume `200`, and a status the provider expresses
    /// through a rendered page (a `429` throttle notice, a `503` maintenance screen) reaches
    /// the caller as a successful fetch of an unparseable document. Server-directed backoff
    /// and adaptive rate limiting would then never see it.
    #[serde(default)]
    pub status: Option<u16>,
    /// The response headers the solver observed, when the back-end reports them; `Retry-After`
    /// is the one that matters.
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    /// The seconds the session is expected to stay valid, which sets the Redis TTL.
    pub ttl_secs: u64,
}

/// The failure modes of a solve attempt.
#[derive(Debug, thiserror::Error)]
pub enum SolveError {
    /// Talking to the solver back-end failed.
    #[error("solver transport error: {0}")]
    Transport(String),
    /// The back-end ran but could not defeat the challenge.
    ///
    /// A statement about the provider: it is challenging us and the challenge held. Reserve it
    /// for that; [`Self::Unavailable`] looks identical from a call site and means the
    /// opposite.
    #[error("solver could not bypass the challenge: {0}")]
    Unsolved(String),
    /// The solver tier cannot serve solves right now: the service is down, its browser pool is
    /// exhausted, or it is shedding load.
    ///
    /// Distinct from [`Self::Unsolved`] because the two lead to opposite conclusions. A
    /// challenge that cannot be beaten is the provider's answer, and re-asking is pointless. A
    /// missing solver says nothing about the provider, and the same request will likely
    /// succeed once it is back. Merging them would make a solver outage look like every
    /// provider turning hostile, and cost each affected fetch its retries.
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
    /// Returns whether another attempt could succeed without anything changing at the provider.
    ///
    /// Only failures of our own tier qualify. [`Self::Unsolved`] does not: the provider
    /// answered, and repeating the exchange re-runs an expensive browser solve for the same
    /// verdict. Nor does [`Self::Malformed`]: an undecodable response is a broken contract,
    /// which a retry reproduces exactly.
    #[must_use]
    pub const fn is_transient(&self) -> bool {
        matches!(
            self,
            Self::Transport(_) | Self::Unavailable(_) | Self::Timeout
        )
    }
}

/// Solves bot-management challenges: [`super::TrawlSolver`] by default, the headless `render`
/// service, or a custom back-end.
///
/// Every back-end sits behind this one method, so the fetch pipeline, adapters and workers
/// never name a concrete solver (`docs/ARCHITECTURE.md` §9).
#[async_trait]
pub trait ChallengeSolver: Send + Sync {
    /// Solves the challenge for `req.url`, returning a reusable session.
    ///
    /// # Errors
    /// [`SolveError::Transport`] or [`SolveError::Timeout`] if the back-end cannot be reached in
    /// time, [`SolveError::Unavailable`] if it cannot serve solves now, [`SolveError::Unsolved`]
    /// if the challenge held, and [`SolveError::Malformed`] for an unreadable answer. `Ok` holds
    /// the session, and the rendered page when the back-end returns one.
    async fn solve(&self, req: SolveRequest) -> Result<SolveOutcome, SolveError>;

    /// Returns a short identifier for the back-end, shown in the console.
    fn backend_name(&self) -> &'static str;
}
