//! Provides the default [`ChallengeSolver`] back-end, a TRAWL client.
//!
//! TRAWL runs as a companion container. The client posts the target URL to its native `/scrape`
//! endpoint; TRAWL walks its tier ladder (plain fetch, cached browser session, fresh solve,
//! residential proxy) and returns the solved session (cookies and user agent), the rendered
//! HTML, the status its winning tier saw upstream, and that tier's response headers. The
//! `challenge-solver` service selects this back-end by config.
//!
//! It uses `/scrape`, not TRAWL's FlareSolverr-compatible `/v1`: that endpoint answers with a
//! hard-coded empty `headers` object, which would drop `Retry-After`, the one header the
//! backoff layer reads.

use crate::types::{ChallengeSolver, SolveError, SolveOutcome, SolveRequest};
use async_trait::async_trait;
use serde::Deserialize;
use std::time::Duration;

/// Solves challenges through TRAWL.
pub struct TrawlSolver {
    client: wreq::Client,
    /// The base endpoint, such as `http://trawl:8191`.
    endpoint: String,
    /// The per-solve time budget handed to TRAWL as `maxTimeout`, in milliseconds.
    max_timeout_ms: u64,
    /// The TTL attached to a solved session in the cache.
    session_ttl_secs: u64,
}

impl TrawlSolver {
    /// Builds a solver against `endpoint` with a per-solve budget and a cache TTL for solved
    /// sessions.
    ///
    /// # Panics
    /// If the HTTP client cannot be built, which means the bundled TLS backend failed to
    /// initialise: a broken binary, caught before the process serves.
    #[must_use]
    pub fn new(endpoint: impl Into<String>, max_timeout_ms: u64, session_ttl_secs: u64) -> Self {
        // A client timeout above TRAWL's own budget, so TRAWL's timeout wins and is reported
        // as `SolveError::Timeout`, not a transport error.
        //
        // No emulation profile: TRAWL is a companion container on the internal network, not a
        // provider to blend in with.
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

/// TRAWL's `/scrape` success document.
///
/// The diagnostic members (`tier`, `timings`, `sessionCached`, `totalMs`) are not read.
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
    /// The upstream response headers, lower-cased; absent when the winning tier recorded
    /// none.
    #[serde(rename = "responseHeaders", default)]
    response_headers: std::collections::HashMap<String, String>,
}

#[derive(Deserialize)]
struct TrawlCookie {
    name: String,
    value: String,
}

/// TRAWL's two error bodies: the native `{"error": …}`, and on pool exhaustion a
/// `FlareSolverr` v2 envelope with its text in `message`.
///
/// Both are decoded, so a failure reaches the caller in the back-end's own words.
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

/// Turns a non-success answer from TRAWL into the failure it describes.
///
/// The split cannot be "is this a 5xx": TRAWL reports its most ordinary outcome, the ladder ran
/// and the challenge held, as a `500` with `{"error": "All tiers exhausted…"}`. Only two shapes
/// mean the tier could not serve the request: `429`, pool saturation, and the gateway statuses,
/// an intermediary or a service that is not up. Both clear on their own; everything else is the
/// provider's answer.
fn classify_failure(status: u16, body: &str) -> SolveError {
    let described = describe_failure(status, body);
    match status {
        429 | 502..=504 => SolveError::Unavailable(described),
        _ => SolveError::Unsolved(described),
    }
}

/// Returns the back-end's description of a failed solve.
///
/// Falls back to the status line when the body is neither TRAWL error shape, usually because an
/// intermediary answered.
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

    /// Mounts TRAWL's native `POST /scrape`, answering `response`.
    async fn mount_scrape(server: &MockServer, response: ResponseTemplate) {
        Mock::given(method("POST"))
            .and(path("/scrape"))
            .respond_with(response)
            .expect(1)
            .mount(server)
            .await;
    }

    /// The request document is exactly the target URL and its time budget.
    ///
    /// `maxTimeout` is TRAWL's spelling, and TRAWL ignores an unknown member, so a rename would
    /// silently run every solve on the 60s default. The provider and kind are not forwarded:
    /// TRAWL has no use for them.
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

    /// Every field maps from TRAWL's shape: `userAgent` to `user_agent`, the cookie object list
    /// to name/value pairs, `statusCode` to `status`, the `responseHeaders` object to pairs.
    ///
    /// `ttl_secs` comes from this deployment's configuration, not the response; read off the
    /// wire it would cache sessions for zero seconds. `status` and `responseHeaders` matter
    /// most: a provider's rendered `429` with a `Retry-After` reaches the backoff layer only
    /// through them.
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

    /// A solve without `responseHeaders` still succeeds.
    ///
    /// TRAWL omits the member when a tier recorded none, hence `#[serde(default)]`. The fetch
    /// stack then reads the status off the page.
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
        assert_eq!(outcome.headers, Vec::<(String, String)>::new());
    }

    /// An empty `html` becomes `None`, not `Some("")`.
    ///
    /// `None` means "a session was solved, fetch the page yourself"; `Some("")` would be a
    /// successfully fetched empty document that no adapter can parse.
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

    /// An exhausted ladder's `{"error": …}` text survives into [`SolveError::Unsolved`].
    ///
    /// The text names the last tier's failure (`http-403`, `timeout`, …) and is the only
    /// description of why the challenge held.
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

    /// A saturated pool is reported with its envelope message, as [`SolveError::Unavailable`].
    ///
    /// Pool exhaustion is TRAWL's one departure from its native error shape: `429` with a
    /// `FlareSolverr` v2 envelope, its text in `message`. It describes this deployment, not the
    /// provider, and clears by itself, so it must be retried and not read as an unbeatable
    /// challenge.
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

    /// A failure body in neither TRAWL shape still names the status.
    ///
    /// The usual source is an intermediary's HTML error page. A gateway status is the
    /// intermediary's failure, never the provider's.
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

    /// An exhausted ladder is the provider's answer, whatever its status.
    ///
    /// TRAWL answers it with a `500`. Reading 5xx as "the tier is down" would make every
    /// unbeatable challenge look like an outage and buy retries that re-run a full browser solve
    /// for the same verdict.
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

    /// A `200` whose body is not TRAWL's shape is malformed, not unsolved: the solver build is
    /// wrong, not the target.
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

    /// A trailing slash on the configured endpoint does not produce `//scrape`.
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
