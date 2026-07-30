//! An HTTP [`ChallengeSolver`] that delegates to the `challenge-solver` microservice.
//!
//! Workers hold one of these; the service fronts the real back-end (`FlareSolverr` by
//! default). Keeping the solve behind the same trait means the worker-side pipeline is
//! identical whether the solver runs in-process or over the network.

use async_trait::async_trait;
use std::time::Duration;
use tankovault_solver::{ChallengeSolver, SolveError, SolveOutcome, SolveRequest};

/// Client for the `challenge-solver` service `POST /v1/solve` endpoint.
pub struct HttpChallengeSolver {
    client: wreq::Client,
    endpoint: String,
    /// Presented as `X-Internal-Token`. The solver refuses unauthenticated callers, so this
    /// is not optional in a production deployment — it is `Option` only because the token is
    /// allowed to be absent outside the production profile.
    token: Option<String>,
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
    pub fn new(endpoint: impl Into<String>, timeout: Duration, token: Option<String>) -> Self {
        // Deliberately no emulation profile and no SSRF resolver: this talks to our own
        // service on the internal network, not to a provider.
        let client = wreq::Client::builder()
            .timeout(timeout)
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
            request = request.header("x-internal-token", token.as_str());
        }
        let resp = request.send().await.map_err(|e| {
            if e.is_timeout() {
                SolveError::Timeout
            } else {
                SolveError::Transport(e.to_string())
            }
        })?;

        if !resp.status().is_success() {
            return Err(SolveError::Unsolved(format!(
                "challenge-solver returned {}",
                resp.status()
            )));
        }
        resp.json::<SolveOutcome>()
            .await
            .map_err(|e| SolveError::Malformed(e.to_string()))
    }

    fn backend_name(&self) -> &'static str {
        "http-challenge-solver"
    }
}
