//! Owns the challenge-solving decorator: it detects a bot-management challenge in-band, delegates
//! to a [`ChallengeSolver`], caches the solved session, and replays it (`docs/ARCHITECTURE.md`
//! §9).
//!
//! Sessions are cached per provider and replayed, so one solve serves many fetches until it
//! expires; expiry triggers a new solve, not a block. The store is a trait, so a Redis-backed one
//! shared across worker replicas can replace the in-memory default.

use crate::error::FetchError;
use crate::fetcher::Fetcher;
use crate::types::{FetchRequest, FetchResponse};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use tankovault_solver::{
    ChallengeSolver, SolveRequest, default_error_page_server, detect_challenge,
    detect_challenge_body, is_rate_limit_page,
};
use time::{Duration, OffsetDateTime};
use tokio::sync::RwLock;

/// The ceiling on the rendered `Cookie` header, in bytes.
///
/// Origins reject an oversized header block outright: nginx answers `400 Request Header Or Cookie
/// Too Large` from its own error page, which looks like any bad request. A solver's jar is a
/// browser's, and on a site with an analytics and ad stack it collects cookies unrelated to
/// clearance that push the header past the origin's buffer.
///
/// Half of nginx's stock 8 KiB buffer, leaving room for the rest of the header block.
const MAX_COOKIE_HEADER_BYTES: usize = 4096;

/// The cookie names carrying bot-management clearance or a server session, in order of
/// preference when the jar does not fit.
///
/// Matched as a case-insensitive prefix, because these are versioned or suffixed in practice
/// (`wordpress_logged_in_<hash>`, `__cf_bm`). Dropping one costs the session; dropping an
/// analytics cookie costs nothing.
const ESSENTIAL_COOKIE_PREFIXES: [&str; 8] = [
    "cf_clearance",
    "__cf",
    "_cf",
    "phpsessid",
    "laravel_session",
    "xsrf-token",
    "wordpress_",
    "session",
];

/// A cached solved session: cookies, user agent and expiry.
#[derive(Debug, Clone)]
pub struct SolvedSession {
    /// The solver's cookie jar as name and value pairs.
    ///
    /// Empty means the solve cleared nothing, and such a session is never replayed.
    pub cookies: Vec<(String, String)>,
    /// The browser identity the cookies were issued to; replaying them under another one
    /// invalidates a `cf_clearance`.
    pub user_agent: String,
    /// When this deployment stops replaying the session, from `solver.session_ttl_secs`; not the
    /// cookies' own expiry.
    pub expires_at: OffsetDateTime,
}

impl SolvedSession {
    fn is_valid(&self) -> bool {
        self.expires_at > OffsetDateTime::now_utc()
    }

    /// Returns whether replaying this session can achieve anything.
    ///
    /// A session with **no cookies** carries only the solver's user agent. Applying it swaps the
    /// provider's browser identity for the solver's on every later request and preserves no
    /// clearance. Solvers return one routinely: a tier that fetched without a browser has no jar.
    ///
    /// A session whose **browser has no emulation profile** cannot be presented coherently. The
    /// fetch layer drops a user agent it cannot match (`base::can_reproduce`), and cookies bound
    /// to an unsent user agent are not honoured. Caching it would only suppress re-solving for
    /// its TTL.
    fn is_replayable(&self) -> bool {
        !self.cookies.is_empty() && crate::base::can_reproduce(&self.user_agent)
    }

    /// Renders the `Cookie` header value, bounded by [`MAX_COOKIE_HEADER_BYTES`].
    ///
    /// Truncation is a last resort and is reported. A header the origin refuses fails every
    /// request while the session is cached, whereas a missing analytics cookie is one the site
    /// never reads.
    fn cookie_header(&self) -> String {
        let rendered = render_cookies(self.cookies.iter());
        if rendered.len() <= MAX_COOKIE_HEADER_BYTES {
            return rendered;
        }

        // Essentials first, then the rest in the solver's order, stopping at the cap, so no
        // cookie is dropped while a less important one is kept.
        let (essential, rest): (Vec<_>, Vec<_>) = self
            .cookies
            .iter()
            .partition(|(name, _)| is_essential(name));
        let mut kept: Vec<&(String, String)> = Vec::with_capacity(self.cookies.len());
        let mut budget = MAX_COOKIE_HEADER_BYTES;
        for cookie in essential.into_iter().chain(rest) {
            // `name=value` plus the "; " separator every cookie after the first costs.
            let cost = cookie.0.len() + 1 + cookie.1.len() + usize::from(!kept.is_empty()) * 2;
            if cost > budget {
                continue;
            }
            budget -= cost;
            kept.push(cookie);
        }
        tracing::warn!(
            rendered_bytes = rendered.len(),
            cap = MAX_COOKIE_HEADER_BYTES,
            cookies = self.cookies.len(),
            kept = kept.len(),
            "solved session cookie jar exceeds the header cap; replaying the essential cookies only"
        );
        render_cookies(kept.into_iter())
    }
}

fn render_cookies<'a, I: Iterator<Item = &'a (String, String)>>(cookies: I) -> String {
    cookies
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("; ")
}

fn is_essential(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    ESSENTIAL_COOKIE_PREFIXES
        .iter()
        .any(|prefix| lower.starts_with(prefix))
}

/// Storage for solved sessions, keyed by provider slug.
#[async_trait]
pub trait SessionStore: Send + Sync {
    /// Returns a still-valid session for `provider`, if any.
    async fn get(&self, provider: &str) -> Option<SolvedSession>;
    /// Stores or replaces the session for `provider`.
    async fn put(&self, provider: &str, session: SolvedSession);
    /// Discards `provider`'s session before it expires.
    ///
    /// A cached session is replayed on every request until its TTL lapses, so one the origin
    /// refuses would fail every fetch until then. This lets the stack drop it and start over.
    async fn invalidate(&self, provider: &str);
}

/// Stores sessions in process memory: the default, for a single replica or tests.
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
    async fn invalidate(&self, provider: &str) {
        self.map.write().await.remove(provider);
    }
}

/// Detects challenges and solves them via the injected solver.
pub struct SolvingFetcher<F> {
    inner: F,
    solver: Arc<dyn ChallengeSolver>,
    store: Arc<dyn SessionStore>,
}

impl<F> SolvingFetcher<F> {
    /// Wraps `inner`, solving through `solver` and caching sessions in `store`.
    ///
    /// Every request reads `store` for a session to replay; only a challenged one calls
    /// `solver`.
    #[must_use]
    pub fn new(inner: F, solver: Arc<dyn ChallengeSolver>, store: Arc<dyn SessionStore>) -> Self {
        Self {
            inner,
            solver,
            store,
        }
    }
}

/// Returns the status to report for a solved page.
///
/// **The page outranks the report.** A solver's status is second-hand, and a browser reports
/// `200` for any completed navigation, a rendered throttle notice included. A document that is
/// the provider's "Too Many Requests" page is a `429` whatever the solver claims, and on a
/// back-end that reports no status it is the only evidence. Believing the report would carry a
/// rendered `429` to an adapter as an unparseable success.
fn solved_status(reported: Option<u16>, html: &str) -> u16 {
    if is_rate_limit_page(html) {
        return 429;
    }
    // A redirect is outranked the other way. The solver's browser follows redirects before it
    // returns, so a reported `3xx` is a hop already taken and the html is the destination.
    // `BaseHttpFetcher` follows redirects too, so believing the report would make a URL such as
    // a Madara `/manga/page/1/` (a 301 to `/manga/`) succeed unsolved and fail solved.
    match reported {
        Some(status) if (300..400).contains(&status) => 200,
        Some(status) => status,
        None => 200,
    }
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

/// Returns whether the origin refused the request because of its headers, not its target.
///
/// `431` says so by status. nginx says it with a `400` and its own error page, whose title names
/// the reason; without reading it, the refusal looks like an ordinary bad request.
fn refuses_our_headers(resp: &FetchResponse) -> bool {
    const REASONS: [&str; 2] = ["header or cookie too large", "header fields too large"];

    if resp.status == 431 {
        return true;
    }
    if resp.status != 400 || default_error_page_server(&resp.body).is_none() {
        return false;
    }
    // Bounded by the size cap `default_error_page_server` has already applied.
    let body = resp.body.to_ascii_lowercase();
    REASONS.iter().any(|reason| body.contains(reason))
}

#[async_trait]
impl<F: Fetcher> Fetcher for SolvingFetcher<F> {
    async fn get(&self, req: FetchRequest) -> Result<FetchResponse, FetchError> {
        // Replays a cached session, keeping the request as it arrived so the recovery below can
        // re-issue it without one.
        let (prepared, replayed) = match self.store.get(&req.provider_slug).await {
            Some(session) => (apply_session(req.clone(), &session), true),
            None => (req.clone(), false),
        };

        let resp = self.inner.get(prepared.clone()).await?;

        if replayed && refuses_our_headers(&resp) {
            tracing::warn!(
                provider = %req.provider_slug,
                status = resp.status,
                url = %resp.url,
                "origin refused the replayed session's headers; dropping the session and \
                 retrying without it"
            );
            self.store.invalidate(&req.provider_slug).await;
            let clean = self.inner.get(req.clone()).await?;
            return self.solve_if_challenged(req, clean).await;
        }

        self.solve_if_challenged(prepared, resp).await
    }
}

impl<F: Fetcher> SolvingFetcher<F> {
    /// Passes `resp` through unless it is a challenge; then solves it and answers with the solved
    /// page or a replay of `req` under the fresh session.
    async fn solve_if_challenged(
        &self,
        req: FetchRequest,
        resp: FetchResponse,
    ) -> Result<FetchResponse, FetchError> {
        let Some(kind) = detect_challenge(&resp) else {
            return Ok(resp);
        };

        tracing::info!(provider = %req.provider_slug, challenge = kind.as_str(), "challenge detected; delegating to solver");
        let solve_started = std::time::Instant::now();
        let solved = self
            .solver
            .solve(SolveRequest {
                url: req.url.clone(),
                provider: req.provider_slug.clone(),
                kind: Some(kind),
            })
            .await;
        // Recorded before the `?`: a failed solve still cost its seconds.
        crate::accounting::record(crate::accounting::Metered::Solve(solve_started.elapsed()));
        let outcome = solved.map_err(|e| {
            if e.is_transient() {
                FetchError::SolverUnavailable(e.to_string())
            } else {
                FetchError::Solver(e.to_string())
            }
        })?;

        let session = SolvedSession {
            cookies: outcome.cookies.clone(),
            user_agent: outcome.user_agent.clone(),
            expires_at: OffsetDateTime::now_utc()
                + Duration::seconds(i64::try_from(outcome.ttl_secs).unwrap_or(i64::MAX)),
        };
        let replayable = session.is_replayable();
        if replayable {
            self.store.put(&req.provider_slug, session.clone()).await;
        } else {
            tracing::debug!(
                provider = %req.provider_slug,
                cookies = session.cookies.len(),
                user_agent = %session.user_agent,
                "solved session is not replayable; not caching it"
            );
        }

        // Uses the solver's page directly, once it is confirmed to be the page and not the
        // interstitial again: a solver that timed out mid-challenge still returns 200-shaped
        // HTML, which an adapter would report as a malformed body.
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

        // Otherwise replays the original request, with the fresh session when it carries
        // anything.
        let replay = if replayable {
            apply_session(req, &session)
        } else {
            req
        };
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

    /// A user agent the deployed solver returns, so a session built with it can be presented.
    const SOLVER_UA: &str =
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:150.0) Gecko/20100101 Firefox/150.0";

    /// Builds a solving stack whose solver reports `cookies` and `user_agent`, plus its store, so
    /// a test can ask what was cached.
    fn solving_session(
        cookies: Vec<(String, String)>,
        user_agent: &str,
    ) -> (SolvingFetcher<Challenged>, Arc<InMemorySessionStore>) {
        let store = Arc::new(InMemorySessionStore::default());
        let solver = StaticSolver::new(SolveOutcome {
            cookies,
            user_agent: user_agent.to_owned(),
            html: Some("<html><body><h1>Chapter 12</h1></body></html>".to_owned()),
            status: Some(200),
            headers: Vec::new(),
            ttl_secs: 900,
        });
        (
            SolvingFetcher::new(Challenged, Arc::new(solver), store.clone()),
            store,
        )
    }

    fn clearance() -> Vec<(String, String)> {
        vec![("cf_clearance".to_owned(), "abc".to_owned())]
    }

    /// A session that can be presented is cached, so one solve amortises over later fetches.
    #[tokio::test]
    async fn a_replayable_session_is_cached() {
        let (fetcher, store) = solving_session(clearance(), SOLVER_UA);
        fetcher.get(req()).await.expect("the page is returned");
        assert!(store.get("example").await.is_some());
    }

    /// A solve that returned **no cookies** is not cached.
    ///
    /// The bug: caching it swaps the provider's browser identity for the solver's on every later
    /// request while preserving no clearance.
    #[tokio::test]
    async fn a_cookieless_session_is_not_cached() {
        let (fetcher, store) = solving_session(Vec::new(), SOLVER_UA);
        fetcher.get(req()).await.expect("the page is returned");
        assert!(
            store.get("example").await.is_none(),
            "a session with no cookies was cached"
        );
    }

    /// A session whose browser has no emulation profile is not cached: its user agent would be
    /// dropped and its cookies not honoured, so caching it only suppresses the next solve.
    #[tokio::test]
    async fn a_session_whose_browser_cannot_be_reproduced_is_not_cached() {
        let (fetcher, store) = solving_session(clearance(), "SolverUA/2.0");
        fetcher.get(req()).await.expect("the page is returned");
        assert!(
            store.get("example").await.is_none(),
            "a session we cannot present was cached"
        );
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
        // A page served with a non-success status is not a successful fetch, however well the
        // challenge was solved; flattening it to 200 would hide a rendered 429 or 503.
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

    /// A reported redirect is a hop the solver already followed.
    ///
    /// The bug: failing on a reported `3xx` rejects the destination document the browser handed
    /// back. `/manga/page/1/` on a Madara site 301s to `/manga/`, so every full scan of a
    /// Cloudflare-gated one fails on its first page.
    #[tokio::test]
    async fn a_reported_redirect_is_a_hop_the_solver_already_followed() {
        let resp = solving_with(
            "<html><head><title>Manga Archive</title></head><body>the destination</body></html>",
            Some(301),
            Vec::new(),
        )
        .get(req())
        .await
        .expect("the destination document is returned, not an error");
        assert_eq!(resp.status, 200);
        assert!(resp.body.contains("the destination"));
    }

    #[tokio::test]
    async fn a_throttle_page_outranks_a_solver_reported_200() {
        // A solver reports 200 for any completed navigation, throttle notice included; the page
        // decides.
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

    /// An oversized jar keeps the cookies that carry the session.
    ///
    /// The bug: a jar too large to send fails every fetch until the session's TTL lapses, with
    /// `400 Request Header Or Cookie Too Large`. The rest is a browser's accumulated analytics.
    #[test]
    fn an_oversized_cookie_jar_keeps_the_cookies_that_carry_the_session() {
        let mut cookies = vec![("cf_clearance".to_owned(), "x".repeat(600))];
        // Enough analytics cookies to blow the cap several times over.
        for i in 0..40 {
            cookies.push((format!("_ga_tracker_{i}"), "v".repeat(300)));
        }
        let session = SolvedSession {
            cookies,
            user_agent: "UA/1.0".to_owned(),
            expires_at: OffsetDateTime::now_utc() + Duration::seconds(600),
        };

        let header = session.cookie_header();
        assert!(
            header.len() <= MAX_COOKIE_HEADER_BYTES,
            "header is {} bytes",
            header.len()
        );
        assert!(
            header.starts_with("cf_clearance="),
            "clearance must survive the truncation: {}",
            &header[..40.min(header.len())]
        );
    }

    /// A jar within the cap travels intact and in the solver's order: a site may care about a
    /// cookie this crate has never heard of.
    #[test]
    fn a_jar_within_the_cap_is_replayed_verbatim() {
        let session = SolvedSession {
            cookies: vec![
                ("_ga".to_owned(), "1".to_owned()),
                ("cf_clearance".to_owned(), "abc".to_owned()),
            ],
            user_agent: "UA/1.0".to_owned(),
            expires_at: OffsetDateTime::now_utc() + Duration::seconds(600),
        };
        assert_eq!(session.cookie_header(), "_ga=1; cf_clearance=abc");
    }

    /// Only a header-size refusal discards a session: a plain `400` is an ordinary bad request,
    /// and dropping the session for one would re-solve on every malformed URL.
    #[test]
    fn only_a_header_size_refusal_counts_as_one() {
        let nginx = |status: u16, reason: &str| FetchResponse {
            status,
            url: "https://example.test/x".to_owned(),
            headers: Vec::new(),
            body: format!(
                "<html><head><title>{status} {reason}</title></head><body>\
                 <center><h1>{status} Bad Request</h1></center><center>{reason}</center>\
                 <hr><center>nginx/1.18.0 (Ubuntu)</center></body></html>"
            ),
            from_cache: false,
        };

        assert!(refuses_our_headers(&nginx(
            400,
            "Request Header Or Cookie Too Large"
        )));
        assert!(!refuses_our_headers(&nginx(400, "Bad Request")));
        assert!(!refuses_our_headers(&nginx(404, "Not Found")));

        // The standard status says it without a body to read.
        assert!(refuses_our_headers(&FetchResponse {
            status: 431,
            url: "https://example.test/x".to_owned(),
            headers: Vec::new(),
            body: String::new(),
            from_cache: false,
        }));
    }

    /// Refuses any request carrying a `Cookie`, like an origin whose header buffer the jar has
    /// outgrown, and serves content to one without.
    struct RefusesCookies;

    #[async_trait]
    impl Fetcher for RefusesCookies {
        async fn get(&self, req: FetchRequest) -> Result<FetchResponse, FetchError> {
            let cookied = req
                .headers
                .iter()
                .any(|(k, _)| k.eq_ignore_ascii_case("cookie"));
            Ok(FetchResponse {
                status: if cookied { 400 } else { 200 },
                url: req.url.clone(),
                headers: Vec::new(),
                body: if cookied {
                    "<html><head><title>400 Request Header Or Cookie Too Large</title></head>\
                     <body><center><h1>400 Bad Request</h1></center>\
                     <center>Request Header Or Cookie Too Large</center>\
                     <hr><center>nginx/1.18.0 (Ubuntu)</center></body></html>"
                        .to_owned()
                } else {
                    "<html><body><h1>Chapter 12</h1></body></html>".to_owned()
                },
                from_cache: false,
            })
        }
    }

    /// A session the origin refuses is dropped and the request retried without it.
    ///
    /// The bug: otherwise the refused session is replayed until its TTL lapses, failing every
    /// fetch with nothing in the failure feed naming the session.
    #[tokio::test]
    async fn a_session_the_origin_refuses_is_dropped_and_the_request_retried_without_it() {
        let store: Arc<dyn SessionStore> = Arc::new(InMemorySessionStore::default());
        store
            .put(
                "example",
                SolvedSession {
                    cookies: vec![("cf_clearance".to_owned(), "x".to_owned())],
                    user_agent: "UA/1.0".to_owned(),
                    expires_at: OffsetDateTime::now_utc() + Duration::seconds(600),
                },
            )
            .await;

        let fetcher = SolvingFetcher::new(
            RefusesCookies,
            Arc::new(StaticSolver::new(SolveOutcome {
                cookies: Vec::new(),
                user_agent: "UA/1.0".to_owned(),
                html: None,
                status: None,
                headers: Vec::new(),
                ttl_secs: 900,
            })),
            Arc::clone(&store),
        );

        let resp = fetcher.get(req()).await.expect("the clean retry succeeds");
        assert_eq!(resp.status, 200);
        assert!(resp.body.contains("Chapter 12"));
        assert!(
            store.get("example").await.is_none(),
            "the refused session must not survive to be replayed again"
        );
    }

    #[tokio::test]
    async fn a_solver_that_hands_back_the_interstitial_has_not_solved_anything() {
        // Accepted as a 200, an unsolved challenge would reach the adapter as a malformed body.
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
