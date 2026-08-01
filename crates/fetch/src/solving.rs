//! Challenge-solving decorator: detect a bot-management challenge in-band, delegate to a
//! [`ChallengeSolver`], cache the solved session, and replay (design §9).
//!
//! Sessions are cached per provider and replayed on later requests so one solve
//! amortises across many fetches until it expires; expiry re-triggers a solve, not a
//! block. The store is abstracted so a Redis-backed implementation (shared across worker
//! replicas) can replace the in-memory default without touching this decorator.

use crate::error::FetchError;
use crate::fetcher::Fetcher;
use crate::types::{FetchRequest, FetchResponse};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use tankovault_solver::{
    ChallengeSolver, SolveRequest, detect_challenge, detect_challenge_body, is_rate_limit_page,
};
use time::{Duration, OffsetDateTime};
use tokio::sync::RwLock;

/// A cached solved session (cookies + UA + expiry).
#[derive(Debug, Clone)]
pub struct SolvedSession {
    pub cookies: Vec<(String, String)>,
    pub user_agent: String,
    pub expires_at: OffsetDateTime,
}

impl SolvedSession {
    fn is_valid(&self) -> bool {
        self.expires_at > OffsetDateTime::now_utc()
    }

    /// Render the `Cookie` header value.
    fn cookie_header(&self) -> String {
        self.cookies
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// Storage for solved sessions, keyed by provider slug.
#[async_trait]
pub trait SessionStore: Send + Sync {
    /// Fetch a still-valid session for `provider`, if any.
    async fn get(&self, provider: &str) -> Option<SolvedSession>;
    /// Store (or replace) the session for `provider`.
    async fn put(&self, provider: &str, session: SolvedSession);
}

/// Process-local session store (default; suitable for a single replica or tests).
#[derive(Default)]
pub struct InMemorySessionStore {
    map: RwLock<HashMap<String, SolvedSession>>,
}

#[async_trait]
impl SessionStore for InMemorySessionStore {
    async fn get(&self, provider: &str) -> Option<SolvedSession> {
        let guard = self.map.read().await;
        guard.get(provider).filter(|s| s.is_valid()).cloned()
    }
    async fn put(&self, provider: &str, session: SolvedSession) {
        self.map.write().await.insert(provider.to_owned(), session);
    }
}

/// Detects challenges and solves them via the injected solver.
pub struct SolvingFetcher<F> {
    inner: F,
    solver: Arc<dyn ChallengeSolver>,
    store: Arc<dyn SessionStore>,
}

impl<F> SolvingFetcher<F> {
    #[must_use]
    pub fn new(inner: F, solver: Arc<dyn ChallengeSolver>, store: Arc<dyn SessionStore>) -> Self {
        Self {
            inner,
            solver,
            store,
        }
    }
}

/// The status to report for a solved page.
///
/// **The page outranks the report.** A solver back-end's status is second-hand and, for the
/// default one, frequently invented: `FlareSolverr` reports `200` for any navigation that
/// completed, throttle notice included, and returns no response headers to contradict it. A
/// document that *is* the provider's "Too Many Requests" page is a `429` whatever the solver
/// claims — and it is the only evidence left once the real status has been discarded upstream.
///
/// Believing the report over the body is what let a rendered `429` travel all the way to an
/// adapter as a successful fetch, where the only available verdict was "unparseable".
fn solved_status(reported: Option<u16>, html: &str) -> u16 {
    if is_rate_limit_page(html) {
        return 429;
    }
    reported.unwrap_or(200)
}

fn apply_session(mut req: FetchRequest, session: &SolvedSession) -> FetchRequest {
    req.user_agent = Some(session.user_agent.clone());
    req.headers
        .retain(|(k, _)| !k.eq_ignore_ascii_case("cookie"));
    if !session.cookies.is_empty() {
        req.headers
            .push(("Cookie".to_owned(), session.cookie_header()));
    }
    req
}

#[async_trait]
impl<F: Fetcher> Fetcher for SolvingFetcher<F> {
    async fn get(&self, mut req: FetchRequest) -> Result<FetchResponse, FetchError> {
        // Replay a cached session if we have one.
        if let Some(session) = self.store.get(&req.provider_slug).await {
            req = apply_session(req, &session);
        }

        let resp = self.inner.get(req.clone()).await?;
        let Some(kind) = detect_challenge(&resp) else {
            return Ok(resp);
        };

        tracing::info!(provider = %req.provider_slug, challenge = kind.as_str(), "challenge detected; delegating to solver");
        let outcome = self
            .solver
            .solve(SolveRequest {
                url: req.url.clone(),
                provider: req.provider_slug.clone(),
                kind: Some(kind),
            })
            .await
            .map_err(|e| FetchError::Solver(e.to_string()))?;

        let session = SolvedSession {
            cookies: outcome.cookies.clone(),
            user_agent: outcome.user_agent.clone(),
            expires_at: OffsetDateTime::now_utc()
                + Duration::seconds(i64::try_from(outcome.ttl_secs).unwrap_or(i64::MAX)),
        };
        self.store.put(&req.provider_slug, session.clone()).await;

        // Use the solver's fetched page directly, but only once it's confirmed to be the
        // *page* and not the interstitial again — a solver that timed out mid-challenge still
        // returns 200-shaped HTML, and passing that off as content reaches the adapter
        // disguised as a malformed body.
        if let Some(html) = outcome.html {
            if let Some(kind) = detect_challenge_body(&html) {
                tracing::warn!(
                    provider = %req.provider_slug,
                    challenge = kind.as_str(),
                    "solver returned a challenge page; treating the fetch as unsolved"
                );
                return Err(FetchError::Challenge(kind));
            }
            let status = solved_status(outcome.status, &html);
            if status >= 400 {
                tracing::debug!(
                    provider = %req.provider_slug,
                    status,
                    "solved fetch carries a non-success status; passing it up the stack"
                );
            }
            return Ok(FetchResponse {
                status,
                url: req.url.clone(),
                headers: outcome.headers,
                body: html,
                from_cache: false,
            });
        }

        // Otherwise replay the original request with the fresh session.
        let replay = apply_session(req, &session);
        let resp2 = self.inner.get(replay).await?;
        if detect_challenge(&resp2).is_some() {
            return Err(FetchError::Challenge(kind));
        }
        Ok(resp2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tankovault_solver::{ChallengeKind, SolveOutcome, StaticSolver};

    /// Serves the Cloudflare interstitial for every request, so a solve is always triggered.
    struct Challenged;

    #[async_trait]
    impl Fetcher for Challenged {
        async fn get(&self, req: FetchRequest) -> Result<FetchResponse, FetchError> {
            Ok(FetchResponse {
                status: 503,
                url: req.url.clone(),
                headers: vec![("server".to_owned(), "cloudflare".to_owned())],
                body: "<html><head><title>Just a moment...</title></head><body>\
                       <script src=\"/cdn-cgi/challenge-platform/h/b/orchestrate\"></script>\
                       </body></html>"
                    .to_owned(),
                from_cache: false,
            })
        }
    }

    fn solving(html: &str) -> SolvingFetcher<Challenged> {
        solving_with(html, None, Vec::new())
    }

    fn solving_with(
        html: &str,
        status: Option<u16>,
        headers: Vec<(String, String)>,
    ) -> SolvingFetcher<Challenged> {
        let solver = StaticSolver::new(SolveOutcome {
            cookies: vec![("cf_clearance".to_owned(), "x".to_owned())],
            user_agent: "UA/1.0".to_owned(),
            html: Some(html.to_owned()),
            status,
            headers,
            ttl_secs: 900,
        });
        SolvingFetcher::new(
            Challenged,
            Arc::new(solver),
            Arc::new(InMemorySessionStore::default()),
        )
    }

    fn req() -> FetchRequest {
        FetchRequest::new("https://example.test/api/items", "example")
    }

    #[tokio::test]
    async fn a_solved_page_reaches_the_caller() {
        let resp = solving("<html><body><pre>{\"ok\":true}</pre></body></html>")
            .get(req())
            .await
            .expect("solved page is returned");
        assert_eq!(resp.status, 200);
        assert!(resp.body.contains("\"ok\":true"));
    }

    #[tokio::test]
    async fn the_solved_status_and_headers_reach_the_caller() {
        // A page the provider served with a non-success status is not a successful fetch,
        // however well the challenge in front of it was solved. Flattening it to 200 hid
        // every rendered 429/503 from the layers that handle exactly those.
        let resp = solving_with(
            "<html><head><title>Down for maintenance</title></head><body>back soon</body></html>",
            Some(503),
            vec![("retry-after".to_owned(), "30".to_owned())],
        )
        .get(req())
        .await
        .expect("the response is returned, not an error");
        assert_eq!(resp.status, 503);
        assert_eq!(resp.header("Retry-After"), Some("30"));
    }

    #[tokio::test]
    async fn a_throttle_page_outranks_a_solver_reported_200() {
        // FlareSolverr reports 200 for any navigation that completed, and sends no headers to
        // contradict it. Believing that over the page in front of us is what kept a rendered
        // 429 arriving at adapters as content.
        let resp = solving_with(
            "<html lang=\"en\"><head><meta charset=\"utf-8\">\
             <title>Too Many Requests</title></head><body></body></html>",
            Some(200),
            Vec::new(),
        )
        .get(req())
        .await
        .expect("the response is returned, not an error");
        assert_eq!(resp.status, 429);
    }

    #[tokio::test]
    async fn a_throttle_page_from_a_status_less_backend_is_read_as_a_429() {
        // The render back-end reports no status, so the page itself is the only evidence.
        let resp = solving(
            "<html lang=\"en\"><head><meta charset=\"utf-8\">\
             <title>Too Many Requests</title></head><body></body></html>",
        )
        .get(req())
        .await
        .expect("the response is returned, not an error");
        assert_eq!(resp.status, 429);
    }

    #[tokio::test]
    async fn an_ordinary_solved_page_from_a_status_less_backend_is_still_a_200() {
        let resp = solving("<html><body><h1>Chapter 12</h1></body></html>")
            .get(req())
            .await
            .expect("solved page is returned");
        assert_eq!(resp.status, 200);
    }

    #[tokio::test]
    async fn a_solver_that_hands_back_the_interstitial_has_not_solved_anything() {
        // Accepting this as a 200 is how an unsolved challenge reaches an adapter disguised
        // as content, which the adapter can only report as a malformed body.
        let err = solving(
            "<html><head><title>Just a moment...</title></head>\
             <body><div class=\"cf-turnstile\"></div></body></html>",
        )
        .get(req())
        .await
        .expect_err("a challenge page is not a solved page");
        assert!(
            matches!(err, FetchError::Challenge(ChallengeKind::Turnstile)),
            "got {err:?}"
        );
    }
}
