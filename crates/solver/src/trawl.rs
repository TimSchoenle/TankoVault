//! The default [`ChallengeSolver`] back-end: a TRAWL client.
//!
//! TRAWL runs as a companion container. We POST the target URL to its native `/scrape`
//! endpoint; it walks its tier ladder (plain fetch → cached browser session → fresh solve →
//! residential proxy) and returns the solved session (cookies + user-agent), the rendered
//! HTML, the status its winning tier saw upstream, and that tier's response headers. The
//! `challenge-solver` service selects this back-end by config; swapping it for another
//! requires no change here or upstream.
//!
//! Native `/scrape` rather than TRAWL's FlareSolverr-compatible `/v1`: the compatibility
//! endpoint answers with a hard-coded empty `headers` object, so `Retry-After` — the one
//! header the backoff layer exists to read — would be discarded at the wire before anything
//! downstream could act on it.

use crate::types::{ChallengeSolver, SolveError, SolveOutcome, SolveRequest};
use async_trait::async_trait;
use serde::Deserialize;
use std::time::Duration;

/// A TRAWL-backed solver.
pub struct TrawlSolver {
    client: wreq::Client,
    /// Base endpoint, e.g. `http://trawl:8191`.
    endpoint: String,
    /// Per-solve time budget handed to TRAWL (`maxTimeout`, ms).
    max_timeout_ms: u64,
    /// TTL to attach to the solved session in the cache.
    session_ttl_secs: u64,
}

impl TrawlSolver {
    /// Construct a solver against `endpoint` with the given per-solve budget and the TTL
    /// solved sessions are cached for.
    ///
    /// # Panics
    /// If the HTTP client cannot be built. That needs the bundled TLS backend to fail to
    /// initialise, which is a broken binary rather than a runtime condition — so it is a panic
    /// at construction, where the process has not started serving, rather than a `Result` every
    /// caller would `unwrap`.
    #[must_use]
    pub fn new(endpoint: impl Into<String>, max_timeout_ms: u64, session_ttl_secs: u64) -> Self {
        // A generous client timeout above TRAWL's own budget, so its timeout wins and is
        // reported as `SolveError::Timeout` rather than a transport error.
        //
        // No emulation profile here: TRAWL is a companion container on the internal network,
        // not a provider to blend in with.
        let client = wreq::Client::builder()
            .timeout(Duration::from_millis(max_timeout_ms + 15_000))
            .build()
            .expect("HTTP client builds with the bundled trust store");
        Self {
            client,
            endpoint: endpoint.into(),
            max_timeout_ms,
            session_ttl_secs,
        }
    }
}

/// TRAWL's `/scrape` success document. The members it also returns for diagnostics — `tier`,
/// `timings`, `sessionCached`, `totalMs` — are not consumed here.
#[derive(Deserialize)]
struct ScrapeResult {
    #[serde(default)]
    cookies: Vec<TrawlCookie>,
    #[serde(rename = "userAgent", default)]
    user_agent: String,
    #[serde(default)]
    html: String,
    /// The status the winning tier received upstream.
    #[serde(rename = "statusCode", default)]
    status_code: Option<u16>,
    /// Upstream response headers, lowercased. Absent when the winning tier recorded none.
    #[serde(rename = "responseHeaders", default)]
    response_headers: std::collections::HashMap<String, String>,
}

#[derive(Deserialize)]
struct TrawlCookie {
    name: String,
    value: String,
}

/// TRAWL's two error bodies: the native `{"error": …}`, and — on pool exhaustion only — a
/// `FlareSolverr` v2 envelope carrying its text in `message`. Both are decoded so a failure
/// reaches the caller in the back-end's own words rather than as a bare status line.
#[derive(Deserialize, Default)]
struct TrawlError {
    #[serde(default)]
    error: String,
    #[serde(default)]
    message: String,
}

#[async_trait]
impl ChallengeSolver for TrawlSolver {
    async fn solve(&self, req: SolveRequest) -> Result<SolveOutcome, SolveError> {
        let url = format!("{}/scrape", self.endpoint.trim_end_matches('/'));
        let payload = serde_json::json!({
            "url": req.url,
            "maxTimeout": self.max_timeout_ms,
        });

        let http = self
            .client
            .post(&url)
            .json(&payload)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    SolveError::Timeout
                } else {
                    SolveError::Transport(e.to_string())
                }
            })?;

        let status = http.status();
        let body = http
            .text()
            .await
            .map_err(|e| SolveError::Malformed(e.to_string()))?;

        if !status.is_success() {
            return Err(classify_failure(status.as_u16(), &body));
        }

        let parsed: ScrapeResult =
            serde_json::from_str(&body).map_err(|e| SolveError::Malformed(e.to_string()))?;

        Ok(SolveOutcome {
            cookies: parsed
                .cookies
                .into_iter()
                .map(|c| (c.name, c.value))
                .collect(),
            user_agent: parsed.user_agent,
            html: if parsed.html.is_empty() {
                None
            } else {
                Some(parsed.html)
            },
            status: parsed.status_code,
            headers: parsed.response_headers.into_iter().collect(),
            ttl_secs: self.session_ttl_secs,
        })
    }

    fn backend_name(&self) -> &'static str {
        "trawl"
    }
}

/// Turn a non-success answer from TRAWL into the failure it actually describes.
///
/// The split cannot be made on "is this a 5xx", because TRAWL reports its *most ordinary*
/// outcome — the tier ladder ran and the challenge held — as a `500` carrying
/// `{"error": "All tiers exhausted…"}`. Only two shapes mean the tier itself could not serve
/// the request: `429`, which is pool saturation (documented in
/// [`describe_failure`]'s `FlareSolverr`-envelope note), and the gateway statuses, which are an
/// intermediary or a service that is not up. Both clear on their own; everything else is the
/// provider's answer.
fn classify_failure(status: u16, body: &str) -> SolveError {
    let described = describe_failure(status, body);
    match status {
        429 | 502..=504 => SolveError::Unavailable(described),
        _ => SolveError::Unsolved(described),
    }
}

/// The back-end's own description of a failed solve, falling back to the status line when the
/// body is neither of TRAWL's error shapes — an intermediary's error page is the usual source.
fn describe_failure(status: u16, body: &str) -> String {
    let parsed = serde_json::from_str::<TrawlError>(body).unwrap_or_default();
    for message in [parsed.error, parsed.message] {
        if !message.is_empty() {
            return message;
        }
    }
    format!("solver returned HTTP {status}")
}

#[cfg(test)]
mod tests {
    use super::{TrawlSolver, classify_failure};
    use crate::types::{ChallengeKind, ChallengeSolver as _, SolveError, SolveRequest};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn request() -> SolveRequest {
        SolveRequest {
            url: "https://provider.example/manga/x".to_owned(),
            provider: "kunmanga".to_owned(),
            kind: Some(ChallengeKind::CloudflareJs),
        }
    }

    /// Mount `POST /scrape` — TRAWL's native endpoint — answering `response`.
    async fn mount_scrape(server: &MockServer, response: ResponseTemplate) {
        Mock::given(method("POST"))
            .and(path("/scrape"))
            .respond_with(response)
            .expect(1)
            .mount(server)
            .await;
    }

    /// The request document, asserted exactly. The camel-cased `maxTimeout` is TRAWL's
    /// spelling, not ours, and an unknown member is ignored rather than reported — so a
    /// rename here would present as "every solve runs on the 60s default" with no clue where.
    /// It also pins that the *provider* and *kind* this workspace carries are deliberately not
    /// forwarded: TRAWL has no use for them.
    #[tokio::test]
    async fn a_solve_posts_the_target_url_and_its_time_budget() {
        let server = MockServer::start().await;
        mount_scrape(
            &server,
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "userAgent": "UA", "cookies": [], "html": "", "statusCode": 200,
            })),
        )
        .await;

        TrawlSolver::new(server.uri(), 45_000, 600)
            .solve(request())
            .await
            .expect("the solve succeeds");

        let requests = server.received_requests().await.expect("request recording");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&requests[0].body).expect("a JSON body"),
            serde_json::json!({
                "url": "https://provider.example/manga/x",
                "maxTimeout": 45_000,
            })
        );
    }

    /// The full mapping out of TRAWL's shape into ours, which is where every rename lives:
    /// `userAgent` → `user_agent`, a cookie *object list* → name/value pairs, `statusCode` →
    /// `status`, a `responseHeaders` *object* → pairs. `ttl_secs` is asserted too, because it
    /// comes from this deployment's configuration and not from the response — a mapping that
    /// read it off the wire would have nothing to read and would silently cache sessions for
    /// zero seconds.
    ///
    /// `status` and `responseHeaders` carry the point of the whole pair: a provider's throttle,
    /// expressed as a *rendered* `429` with a `Retry-After`, reaches the backoff layer only if
    /// it survives this conversion.
    #[tokio::test]
    async fn a_solved_session_maps_every_field_including_the_rendered_status() {
        let server = MockServer::start().await;
        mount_scrape(
            &server,
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "url": "https://provider.example/manga/x",
                "userAgent": "Mozilla/5.0",
                "cookies": [
                    { "name": "cf_clearance", "value": "abc", "domain": ".example" },
                ],
                "html": "<html>ok</html>",
                "statusCode": 429,
                "responseHeaders": { "retry-after": "30" },
                "tier": 3,
                "sessionCached": false,
                "timings": [{ "tier": 3, "status": "success", "durationMs": 512 }],
                "totalMs": 600,
            })),
        )
        .await;

        let outcome = TrawlSolver::new(server.uri(), 45_000, 600)
            .solve(request())
            .await
            .expect("the solve succeeds");

        assert_eq!(
            outcome.cookies,
            vec![("cf_clearance".to_owned(), "abc".to_owned())]
        );
        assert_eq!(outcome.user_agent, "Mozilla/5.0");
        assert_eq!(outcome.html.as_deref(), Some("<html>ok</html>"));
        assert_eq!(outcome.status, Some(429));
        assert_eq!(
            outcome.headers,
            vec![("retry-after".to_owned(), "30".to_owned())]
        );
        assert_eq!(outcome.ttl_secs, 600);
    }

    /// `responseHeaders` is optional in TRAWL's own type and a tier that recorded none omits
    /// it, which is why it is `#[serde(default)]`. Such a solve must still succeed — the fetch
    /// stack falls back to reading the status off the page, which is worse than knowing but far
    /// better than every solve failing to decode.
    #[tokio::test]
    async fn a_response_without_headers_still_solves() {
        let server = MockServer::start().await;
        mount_scrape(
            &server,
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "userAgent": "Mozilla/5.0",
                "cookies": [],
                "html": "<html>ok</html>",
                "statusCode": 200,
            })),
        )
        .await;

        let outcome = TrawlSolver::new(server.uri(), 45_000, 600)
            .solve(request())
            .await
            .expect("a response without headers still solves");
        assert_eq!(outcome.status, Some(200));
        assert!(outcome.headers.is_empty());
    }

    /// An empty `html` becomes `None` rather than `Some("")`. The two mean different things
    /// downstream: `None` is "a session was solved, fetch the page yourself", and `Some("")`
    /// would be a successfully fetched empty document that no adapter can parse.
    #[tokio::test]
    async fn an_empty_rendered_body_is_absent_rather_than_empty() {
        let server = MockServer::start().await;
        mount_scrape(
            &server,
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "userAgent": "UA", "cookies": [], "html": "", "statusCode": 200,
            })),
        )
        .await;

        let outcome = TrawlSolver::new(server.uri(), 45_000, 600)
            .solve(request())
            .await
            .expect("the solve succeeds");
        assert_eq!(outcome.html, None);
    }

    /// TRAWL reports an exhausted tier ladder as `500` with its own `{"error": …}`, and that
    /// text names the last tier's failure (`http-403`, `timeout`, …). It is the only description
    /// of why the challenge held, so it has to survive into [`SolveError::Unsolved`] rather than
    /// being replaced by the status line.
    #[tokio::test]
    async fn a_failure_is_reported_with_the_back_ends_own_message() {
        let server = MockServer::start().await;
        mount_scrape(
            &server,
            ResponseTemplate::new(500).set_body_json(serde_json::json!({
                "error": "All tiers exhausted. Last failure: http-403",
                "timings": [{ "tier": 4, "status": "blocked", "durationMs": 7890 }],
            })),
        )
        .await;

        let err = TrawlSolver::new(server.uri(), 45_000, 600)
            .solve(request())
            .await
            .expect_err("a non-success status is a failed solve");
        match err {
            SolveError::Unsolved(message) => {
                assert_eq!(message, "All tiers exhausted. Last failure: http-403");
            }
            other => panic!("expected Unsolved, got {other:?}"),
        }
    }

    /// Pool exhaustion is TRAWL's *one* departure from its native error shape: `429` with a
    /// `FlareSolverr` v2 envelope, whose text is in `message` and not `error`. Decoding only the
    /// native shape would turn the most operationally interesting failure there is — "every
    /// browser is busy" — into an anonymous status line.
    ///
    /// It is also [`SolveError::Unavailable`], not `Unsolved`: every browser being busy is a
    /// statement about this deployment, not about the provider, and it clears by itself. Filed
    /// as `Unsolved` it was neither retried nor distinguishable, in the console, from a provider
    /// whose challenge we cannot beat.
    #[tokio::test]
    async fn a_saturated_pool_is_reported_with_its_envelope_message() {
        let server = MockServer::start().await;
        mount_scrape(
            &server,
            ResponseTemplate::new(429).set_body_json(serde_json::json!({
                "status": "error",
                "message": "Browser pool saturated, retry shortly",
                "version": "2.0.0",
                "solution": { "url": "", "status": 0, "headers": {}, "response": "",
                              "cookies": [], "userAgent": "" },
            })),
        )
        .await;

        let err = TrawlSolver::new(server.uri(), 45_000, 600)
            .solve(request())
            .await
            .expect_err("a saturated pool is a failed solve");
        match err {
            SolveError::Unavailable(message) => {
                assert_eq!(message, "Browser pool saturated, retry shortly");
            }
            other => panic!("expected Unavailable, got {other:?}"),
        }
        assert!(
            SolveError::Unavailable(String::new()).is_transient(),
            "a busy pool is exactly the failure another attempt clears"
        );
    }

    /// A failure body that is neither TRAWL error shape — an intermediary's HTML error page is
    /// the usual source — still has to name the status, or the log line says nothing at all.
    ///
    /// A gateway status is also the intermediary's failure, never the provider's: whatever sits
    /// between the worker and the browser could not deliver the request at all.
    #[tokio::test]
    async fn an_unrecognised_failure_body_falls_back_to_the_status() {
        let server = MockServer::start().await;
        mount_scrape(
            &server,
            ResponseTemplate::new(502).set_body_string("<html>Bad Gateway</html>"),
        )
        .await;

        let err = TrawlSolver::new(server.uri(), 45_000, 600)
            .solve(request())
            .await
            .expect_err("a bad gateway is a failed solve");
        match err {
            SolveError::Unavailable(message) => {
                assert!(message.contains("502"), "no status in: {message}");
            }
            other => panic!("expected Unavailable, got {other:?}"),
        }
    }

    /// The line the split turns on, and the one a status-class rule would get backwards: TRAWL
    /// answers its most ordinary failure — the ladder ran and the challenge held — with a `500`.
    /// Reading 5xx as "the tier is down" would make every unbeatable challenge look like an
    /// outage and buy it retries that re-run a full browser solve for the same verdict.
    #[test]
    fn an_exhausted_tier_ladder_is_the_providers_answer_however_it_is_statused() {
        assert!(matches!(
            classify_failure(
                500,
                "{\"error\":\"All tiers exhausted. Last failure: http-403\"}"
            ),
            SolveError::Unsolved(_)
        ));
        assert!(matches!(
            classify_failure(429, "{\"message\":\"Browser pool saturated\"}"),
            SolveError::Unavailable(_)
        ));
        for gateway in [502, 503, 504] {
            assert!(
                matches!(classify_failure(gateway, ""), SolveError::Unavailable(_)),
                "{gateway} is an intermediary failing, not a provider answering"
            );
        }
    }

    /// A `200` whose body is not TRAWL's shape at all is malformed rather than unsolved: nothing
    /// is wrong with the target, the solver build is wrong.
    #[tokio::test]
    async fn a_success_that_is_not_trawls_shape_is_malformed() {
        let server = MockServer::start().await;
        mount_scrape(
            &server,
            ResponseTemplate::new(200).set_body_string("<html>hello</html>"),
        )
        .await;

        let err = TrawlSolver::new(server.uri(), 45_000, 600)
            .solve(request())
            .await
            .expect_err("an undecodable success body is an error");
        assert!(
            matches!(err, SolveError::Malformed(_)),
            "expected Malformed, got {err:?}"
        );
    }

    /// The endpoint comes from configuration, where a trailing slash is the most ordinary typo
    /// there is; without the trim it produces `//scrape`.
    #[tokio::test]
    async fn a_trailing_slash_on_the_endpoint_does_not_double_up() {
        let server = MockServer::start().await;
        mount_scrape(
            &server,
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "userAgent": "UA", "cookies": [], "html": "", "statusCode": 200,
            })),
        )
        .await;

        TrawlSolver::new(format!("{}/", server.uri()), 45_000, 600)
            .solve(request())
            .await
            .expect("the solve succeeds");

        let requests = server.received_requests().await.expect("request recording");
        assert_eq!(requests[0].url.path(), "/scrape");
    }
}
