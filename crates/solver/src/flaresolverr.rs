//! The default [`ChallengeSolver`] back-end: a `FlareSolverr` client.
//!
//! `FlareSolverr` runs as a companion container. We POST `request.get` with the target
//! URL; it drives a headless browser through the challenge and returns the solved
//! session (cookies + user-agent) and HTML. The `challenge-solver` service selects this
//! back-end by config; swapping it for another requires no change here or upstream.

use crate::types::{ChallengeSolver, SolveError, SolveOutcome, SolveRequest};
use async_trait::async_trait;
use serde::Deserialize;
use std::time::Duration;

/// A FlareSolverr-backed solver.
pub struct FlareSolverrSolver {
    client: wreq::Client,
    /// Base endpoint, e.g. `http://flaresolverr:8191`.
    endpoint: String,
    /// Per-solve time budget handed to `FlareSolverr` (`maxTimeout`, ms).
    max_timeout_ms: u64,
    /// TTL to attach to the solved session in the cache.
    session_ttl_secs: u64,
}

impl FlareSolverrSolver {
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
        // A generous client timeout above FlareSolverr's own budget, so its timeout wins
        // and is reported as `SolveError::Timeout` rather than a transport error.
        //
        // No emulation profile here: `FlareSolverr` is a companion container on the internal
        // network, not a provider to blend in with.
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

#[derive(Deserialize)]
struct FsResponse {
    status: String,
    #[serde(default)]
    message: String,
    solution: Option<FsSolution>,
}

#[derive(Deserialize)]
struct FsSolution {
    #[serde(default)]
    cookies: Vec<FsCookie>,
    #[serde(rename = "userAgent", default)]
    user_agent: String,
    #[serde(default)]
    response: String,
    /// Status the browser received for the final navigation. Optional: older builds omit it.
    #[serde(default)]
    status: Option<u16>,
    /// Response headers, as an object. Optional and often empty.
    #[serde(default)]
    headers: std::collections::HashMap<String, String>,
}

#[derive(Deserialize)]
struct FsCookie {
    name: String,
    value: String,
}

#[async_trait]
impl ChallengeSolver for FlareSolverrSolver {
    async fn solve(&self, req: SolveRequest) -> Result<SolveOutcome, SolveError> {
        let url = format!("{}/v1", self.endpoint.trim_end_matches('/'));
        let payload = serde_json::json!({
            "cmd": "request.get",
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

        let parsed: FsResponse = http
            .json::<FsResponse>()
            .await
            .map_err(|e| SolveError::Malformed(e.to_string()))?;

        if parsed.status != "ok" {
            return Err(SolveError::Unsolved(parsed.message));
        }
        let solution = parsed
            .solution
            .ok_or_else(|| SolveError::Malformed("missing solution".to_owned()))?;

        Ok(SolveOutcome {
            cookies: solution
                .cookies
                .into_iter()
                .map(|c| (c.name, c.value))
                .collect(),
            user_agent: solution.user_agent,
            html: if solution.response.is_empty() {
                None
            } else {
                Some(solution.response)
            },
            status: solution.status,
            headers: solution.headers.into_iter().collect(),
            ttl_secs: self.session_ttl_secs,
        })
    }

    fn backend_name(&self) -> &'static str {
        "flaresolverr"
    }
}

#[cfg(test)]
mod tests {
    use super::FlareSolverrSolver;
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

    /// Mount `POST /v1` — `FlareSolverr`'s single endpoint — answering `response`.
    async fn mount_v1(server: &MockServer, response: ResponseTemplate) {
        Mock::given(method("POST"))
            .and(path("/v1"))
            .respond_with(response)
            .expect(1)
            .mount(server)
            .await;
    }

    /// The request document, asserted exactly. `cmd` and the camel-cased `maxTimeout` are
    /// `FlareSolverr`'s spelling, not ours, and it answers a misspelt member with a generic
    /// error rather than by naming it — so a rename here would present as "every solve fails"
    /// with no clue where (F-09). It also pins that the *provider* and *kind* this workspace
    /// carries are deliberately not forwarded: `FlareSolverr` has no use for them.
    #[tokio::test]
    async fn a_solve_posts_the_request_get_command_for_the_target_url() {
        let server = MockServer::start().await;
        mount_v1(
            &server,
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "ok",
                "solution": { "userAgent": "UA", "cookies": [], "response": "" },
            })),
        )
        .await;

        FlareSolverrSolver::new(server.uri(), 45_000, 600)
            .solve(request())
            .await
            .expect("the solve succeeds");

        let requests = server.received_requests().await.expect("request recording");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&requests[0].body).expect("a JSON body"),
            serde_json::json!({
                "cmd": "request.get",
                "url": "https://provider.example/manga/x",
                "maxTimeout": 45_000,
            })
        );
    }

    /// The full mapping out of `FlareSolverr`'s shape into ours, which is where every rename
    /// lives: `userAgent` → `user_agent`, a cookie *object list* → name/value pairs, `response`
    /// → `html`, a headers *object* → pairs. `ttl_secs` is asserted too, because it comes from
    /// this deployment's configuration and not from the response — a mapping that read it off
    /// the wire would have nothing to read and would silently cache sessions for zero seconds.
    ///
    /// `status` carries the point of the whole field: a provider's throttle expressed as a
    /// *rendered* `429` reaches the backoff layer only if it survives this conversion.
    #[tokio::test]
    async fn a_solved_session_maps_every_field_including_the_rendered_status() {
        let server = MockServer::start().await;
        mount_v1(
            &server,
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "ok",
                "solution": {
                    "userAgent": "Mozilla/5.0",
                    "cookies": [
                        { "name": "cf_clearance", "value": "abc", "domain": ".example" },
                    ],
                    "response": "<html>ok</html>",
                    "status": 429,
                    "headers": { "retry-after": "30" },
                },
            })),
        )
        .await;

        let outcome = FlareSolverrSolver::new(server.uri(), 45_000, 600)
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

    /// Older `FlareSolverr` builds omit `status` and `headers` entirely, which is why both are
    /// `#[serde(default)]`. A deployment on one of those must still solve — the fetch stack falls
    /// back to assuming `200`, which is worse than knowing but far better than every solve
    /// failing to decode.
    #[tokio::test]
    async fn a_response_without_status_or_headers_still_solves() {
        let server = MockServer::start().await;
        mount_v1(
            &server,
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "ok",
                "solution": {
                    "userAgent": "Mozilla/5.0",
                    "cookies": [],
                    "response": "<html>ok</html>",
                },
            })),
        )
        .await;

        let outcome = FlareSolverrSolver::new(server.uri(), 45_000, 600)
            .solve(request())
            .await
            .expect("an older build's response still solves");
        assert_eq!(outcome.status, None);
        assert!(outcome.headers.is_empty());
    }

    /// An empty `response` becomes `None` rather than `Some("")`. The two mean different things
    /// downstream: `None` is "a session was solved, fetch the page yourself", and `Some("")` would
    /// be a successfully fetched empty document that no adapter can parse.
    #[tokio::test]
    async fn an_empty_rendered_body_is_absent_rather_than_empty() {
        let server = MockServer::start().await;
        mount_v1(
            &server,
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "ok",
                "solution": { "userAgent": "UA", "cookies": [], "response": "" },
            })),
        )
        .await;

        let outcome = FlareSolverrSolver::new(server.uri(), 45_000, 600)
            .solve(request())
            .await
            .expect("the solve succeeds");
        assert_eq!(outcome.html, None);
    }

    /// `FlareSolverr` reports failure **in the body, with a non-2xx status**, so this client
    /// deliberately decodes before looking at the status line — checking `is_success()` first
    /// would throw away `message`, which is the only description of why the challenge held. That
    /// is the opposite of the convention [`crate::ChallengeSolver`]'s HTTP sibling follows, and
    /// it is deliberate on both sides; the test exists so the difference is not "corrected".
    #[tokio::test]
    async fn a_failure_is_reported_with_the_back_ends_own_message() {
        let server = MockServer::start().await;
        mount_v1(
            &server,
            ResponseTemplate::new(500).set_body_json(serde_json::json!({
                "status": "error",
                "message": "Challenge not detected",
            })),
        )
        .await;

        let err = FlareSolverrSolver::new(server.uri(), 45_000, 600)
            .solve(request())
            .await
            .expect_err("a non-ok status is a failed solve");
        match err {
            SolveError::Unsolved(message) => assert_eq!(message, "Challenge not detected"),
            other => panic!("expected Unsolved, got {other:?}"),
        }
    }

    /// `status: "ok"` with no `solution` is the back-end contradicting itself, and it is a
    /// different fact from an unsolved challenge: nothing is wrong with the target, the solver
    /// build is wrong.
    #[tokio::test]
    async fn an_ok_status_without_a_solution_is_malformed() {
        let server = MockServer::start().await;
        mount_v1(
            &server,
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({ "status": "ok", "message": "" })),
        )
        .await;

        let err = FlareSolverrSolver::new(server.uri(), 45_000, 600)
            .solve(request())
            .await
            .expect_err("ok without a solution is an error");
        match err {
            SolveError::Malformed(message) => assert_eq!(message, "missing solution"),
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    /// A body that is not `FlareSolverr`'s shape at all — an intermediary's error page is the
    /// usual source — is malformed rather than unsolved.
    #[tokio::test]
    async fn a_non_flaresolverr_body_is_malformed() {
        let server = MockServer::start().await;
        mount_v1(
            &server,
            ResponseTemplate::new(502).set_body_string("<html>Bad Gateway</html>"),
        )
        .await;

        let err = FlareSolverrSolver::new(server.uri(), 45_000, 600)
            .solve(request())
            .await
            .expect_err("an undecodable body is an error");
        assert!(
            matches!(err, SolveError::Malformed(_)),
            "expected Malformed, got {err:?}"
        );
    }

    /// The endpoint comes from configuration, where a trailing slash is the most ordinary typo
    /// there is; without the trim it produces `//v1`.
    #[tokio::test]
    async fn a_trailing_slash_on_the_endpoint_does_not_double_up() {
        let server = MockServer::start().await;
        mount_v1(
            &server,
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "ok",
                "solution": { "userAgent": "UA", "cookies": [], "response": "" },
            })),
        )
        .await;

        FlareSolverrSolver::new(format!("{}/", server.uri()), 45_000, 600)
            .solve(request())
            .await
            .expect("the solve succeeds");

        let requests = server.received_requests().await.expect("request recording");
        assert_eq!(requests[0].url.path(), "/v1");
    }
}
