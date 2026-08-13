//! An HTTP [`ChallengeSolver`] that delegates to the `challenge-solver` microservice.
//!
//! Workers hold one of these; the service fronts the real back-end (TRAWL by
//! default). Keeping the solve behind the same trait means the worker-side pipeline is
//! identical whether the solver runs in-process or over the network.

use async_trait::async_trait;
use secrecy::{ExposeSecret as _, SecretString};
use std::time::Duration;
use tankovault_solver::{ChallengeSolver, SolveError, SolveOutcome, SolveRequest};

/// Client for the `challenge-solver` service `POST /v1/solve` endpoint.
pub struct HttpChallengeSolver {
    client: wreq::Client,
    endpoint: String,
    /// Presented as `X-Internal-Token`. The solver refuses unauthenticated callers, so this
    /// is not optional in a production deployment — it is `Option` only because the token is
    /// allowed to be absent outside the production profile.
    token: Option<SecretString>,
}

impl HttpChallengeSolver {
    /// Build a client for `endpoint` (e.g. `http://challenge-solver:8080`) with an
    /// overall solve timeout, presenting `token` to the solver.
    ///
    /// # Panics
    /// If the HTTP client cannot be built. That needs the bundled TLS backend to fail to
    /// initialise, which is a broken binary rather than a runtime condition — so it is a panic
    /// at construction, where the process has not started serving, rather than a `Result` every
    /// caller would `unwrap`.
    #[must_use]
    pub fn new(
        endpoint: impl Into<String>,
        timeout: Duration,
        token: Option<SecretString>,
    ) -> Self {
        Self::build(endpoint, timeout, token, None)
    }

    /// As [`Self::new`], additionally presenting a client certificate.
    ///
    /// The mTLS counterpart, used when `internal.identity = "mtls"`. This hop runs on `wreq`
    /// rather than `reqwest` because the crate it lives in is the crawl stack; the two take
    /// their PEM differently, which is why `tankovault_service::ClientMaterial` hands over bytes
    /// rather than a built client.
    ///
    /// # Panics
    /// As [`Self::new`], and additionally if `material` is not valid PEM — which is a
    /// misconfigured mount, caught at boot before the process serves anything.
    ///
    /// `from_pkcs8_pem` is the strictest consumer of the three in this workspace: it tests the
    /// first bytes of the key against the `PRIVATE KEY` banner PKCS#8 carries, where rustls and
    /// reqwest take PKCS#1 and SEC1 keys as well. `ClientMaterial` therefore hands over a key already
    /// re-encoded as PKCS#8, and a mount this rejects has already failed when the configuration
    /// resolved, with the path in the message.
    #[must_use]
    pub fn with_mtls(
        endpoint: impl Into<String>,
        timeout: Duration,
        material: &tankovault_service::ClientMaterial,
    ) -> Self {
        Self::build(endpoint, timeout, None, Some(material))
    }

    fn build(
        endpoint: impl Into<String>,
        timeout: Duration,
        token: Option<SecretString>,
        material: Option<&tankovault_service::ClientMaterial>,
    ) -> Self {
        // Deliberately no emulation profile and no SSRF resolver: this talks to our own
        // service on the internal network, not to a provider.
        let mut builder = wreq::Client::builder().timeout(timeout);

        if let Some(material) = material {
            builder = builder
                .tls_identity(
                    wreq::tls::trust::Identity::from_pkcs8_pem(&material.cert, &material.key)
                        .expect("ClientMaterial holds a certificate and a PKCS#8 key"),
                )
                // Only the internal authority, never the public roots: a solver endpoint signed
                // by a public CA is not this deployment's solver.
                .tls_cert_store(
                    wreq::tls::trust::CertStore::from_pem_stack(&material.ca)
                        .expect("the mounted CA bundle is valid PEM"),
                );
        }

        let client = builder
            .build()
            .expect("HTTP client builds with the bundled trust store");
        Self {
            client,
            endpoint: endpoint.into(),
            token,
        }
    }
}

#[async_trait]
impl ChallengeSolver for HttpChallengeSolver {
    async fn solve(&self, req: SolveRequest) -> Result<SolveOutcome, SolveError> {
        let url = format!("{}/v1/solve", self.endpoint.trim_end_matches('/'));
        let mut request = self.client.post(&url).json(&req);
        if let Some(token) = &self.token {
            request = request.header("x-internal-token", token.expose_secret());
        }
        let resp = request.send().await.map_err(|e| {
            if e.is_timeout() {
                SolveError::Timeout
            } else {
                SolveError::Transport(e.to_string())
            }
        })?;

        let status = resp.status();
        if !status.is_success() {
            let described = format!("challenge-solver returned {status}");
            // The service answers `5xx` (and a saturated gateway's `429`) when the tier could not
            // serve the solve, and `4xx` when it served it and the challenge held —
            // `tankovault_solver::http` owns that mapping and documents why the two ranges are
            // kept apart. Reading every non-success as `Unsolved` is what reported a solver
            // outage as "solver could not bypass the challenge" against providers that were, at
            // that moment, answering `200` to a plain request.
            return Err(if status.is_server_error() || status.as_u16() == 429 {
                SolveError::Unavailable(described)
            } else {
                SolveError::Unsolved(described)
            });
        }
        resp.json::<SolveOutcome>()
            .await
            .map_err(|e| SolveError::Malformed(e.to_string()))
    }

    fn backend_name(&self) -> &'static str {
        "http-challenge-solver"
    }
}

#[cfg(test)]
mod tests {
    use super::HttpChallengeSolver;
    use secrecy::SecretString;
    use std::time::{Duration, Instant};
    use tankovault_solver::{ChallengeKind, ChallengeSolver as _, SolveError, SolveRequest};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn request() -> SolveRequest {
        SolveRequest {
            url: "https://provider.example/manga/x".to_owned(),
            provider: "kunmanga".to_owned(),
            kind: Some(ChallengeKind::CloudflareJs),
        }
    }

    /// A body the service would send back for a fully-solved page.
    fn solved_body() -> serde_json::Value {
        serde_json::json!({
            "cookies": [["cf_clearance", "abc"]],
            "user_agent": "Mozilla/5.0",
            "html": "<html>ok</html>",
            "status": 429,
            "headers": [["retry-after", "30"]],
            "ttl_secs": 600,
        })
    }

    async fn mount_solve(server: &MockServer, response: ResponseTemplate) {
        Mock::given(method("POST"))
            .and(path("/v1/solve"))
            .respond_with(response)
            .expect(1)
            .mount(server)
            .await;
    }

    /// The whole round trip: the request document, the path, and every field of the outcome.
    ///
    /// `status` and `headers` are asserted deliberately. They exist so a provider's *rendered*
    /// `429` reaches the backoff and rate-limit layers instead of arriving as a successful fetch
    /// of an unparseable document, and this hop is where they would be lost — both are
    /// `#[serde(default)]`, so a name that stopped matching would decode to `None`/empty and
    /// every other assertion here would still pass (F-09).
    #[tokio::test]
    async fn a_solve_posts_the_request_and_returns_every_field_of_the_outcome() {
        let server = MockServer::start().await;
        mount_solve(
            &server,
            ResponseTemplate::new(200).set_body_json(solved_body()),
        )
        .await;

        let outcome = HttpChallengeSolver::new(server.uri(), Duration::from_secs(5), None)
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

        let requests = server.received_requests().await.expect("request recording");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&requests[0].body).expect("a JSON body"),
            serde_json::json!({
                "url": "https://provider.example/manga/x",
                "provider": "kunmanga",
                "kind": "cloudflare_js",
            })
        );
    }

    /// The solver refuses unauthenticated callers (SEC-1/SEC-2), and this client is the only
    /// thing that presents the credential. Nothing checked that it does — a token accepted by the
    /// constructor and then dropped would fail every solve in production with a `403` while every
    /// test in the workspace passed.
    #[tokio::test]
    async fn the_internal_token_is_presented_when_one_is_configured() {
        let server = MockServer::start().await;
        mount_solve(
            &server,
            ResponseTemplate::new(200).set_body_json(solved_body()),
        )
        .await;

        HttpChallengeSolver::new(
            server.uri(),
            Duration::from_secs(5),
            Some(SecretString::from("shared-secret")),
        )
        .solve(request())
        .await
        .expect("the solve succeeds");

        let requests = server.received_requests().await.expect("request recording");
        assert_eq!(
            requests[0]
                .headers
                .get("x-internal-token")
                .map(|v| v.to_str().expect("ASCII header")),
            Some("shared-secret")
        );
    }

    /// The inverse leg. `None` is legal outside the production profile, and it must mean *no
    /// header* rather than an empty one — a solver comparing an empty token against an empty
    /// configured value would authenticate anybody.
    #[tokio::test]
    async fn no_token_means_no_header_rather_than_an_empty_one() {
        let server = MockServer::start().await;
        mount_solve(
            &server,
            ResponseTemplate::new(200).set_body_json(solved_body()),
        )
        .await;

        HttpChallengeSolver::new(server.uri(), Duration::from_secs(5), None)
            .solve(request())
            .await
            .expect("the solve succeeds");

        let requests = server.received_requests().await.expect("request recording");
        assert!(requests[0].headers.get("x-internal-token").is_none());
    }

    /// The endpoint comes from configuration, where a trailing slash is the most ordinary typo
    /// there is. Without the trim it produces `//v1/solve`, which axum does not route.
    #[tokio::test]
    async fn a_trailing_slash_on_the_endpoint_does_not_double_up() {
        let server = MockServer::start().await;
        mount_solve(
            &server,
            ResponseTemplate::new(200).set_body_json(solved_body()),
        )
        .await;

        HttpChallengeSolver::new(format!("{}/", server.uri()), Duration::from_secs(5), None)
            .solve(request())
            .await
            .expect("the solve succeeds");

        let requests = server.received_requests().await.expect("request recording");
        assert_eq!(requests[0].url.path(), "/v1/solve");
    }

    /// A refused or failed solve is [`SolveError::Unsolved`], not [`SolveError::Transport`]: the
    /// service answered, so the network is fine and retrying the *transport* would be wrong.
    #[tokio::test]
    async fn a_non_success_status_is_unsolved_and_names_the_status() {
        let server = MockServer::start().await;
        mount_solve(&server, ResponseTemplate::new(403)).await;

        let err = HttpChallengeSolver::new(server.uri(), Duration::from_secs(5), None)
            .solve(request())
            .await
            .expect_err("a 403 is a failed solve");
        match err {
            SolveError::Unsolved(message) => {
                assert!(message.contains("403"), "no status in: {message}");
            }
            other => panic!("expected Unsolved, got {other:?}"),
        }
    }

    /// A `200` carrying something that is not a [`SolveOutcome`] is the service's contract
    /// breaking, which is a different fact from the challenge being unbeatable.
    ///
    /// [`SolveOutcome`]: tankovault_solver::SolveOutcome
    #[tokio::test]
    async fn an_undecodable_body_is_malformed_rather_than_unsolved() {
        let server = MockServer::start().await;
        mount_solve(
            &server,
            ResponseTemplate::new(200).set_body_string("not json"),
        )
        .await;

        let err = HttpChallengeSolver::new(server.uri(), Duration::from_secs(5), None)
            .solve(request())
            .await
            .expect_err("an undecodable body is an error");
        assert!(
            matches!(err, SolveError::Malformed(_)),
            "expected Malformed, got {err:?}"
        );
    }

    /// A timeout is its own variant rather than a `Transport` string, because the caller treats
    /// them differently: a solve that ran out of budget may be worth another attempt on a longer
    /// one, a transport failure means the service is not there. The distinction is made by
    /// `wreq::Error::is_timeout`, and the branch that reads it is what this pins.
    #[tokio::test]
    async fn exceeding_the_timeout_is_timeout_rather_than_a_transport_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/solve"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(30)))
            .mount(&server)
            .await;

        let started = Instant::now();
        let err = HttpChallengeSolver::new(server.uri(), Duration::from_millis(300), None)
            .solve(request())
            .await
            .expect_err("the solve outlives the timeout");
        assert!(
            matches!(err, SolveError::Timeout),
            "expected Timeout, got {err:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the timeout was not applied: {:?}",
            started.elapsed()
        );
    }
}
