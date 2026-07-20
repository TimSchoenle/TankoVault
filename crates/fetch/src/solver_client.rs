//! An HTTP [`ChallengeSolver`] that delegates to the `challenge-solver` microservice.
//!
//! Workers hold one of these; the service fronts the real back-end (`FlareSolverr` by
//! default). Keeping the solve behind the same trait means the worker-side pipeline is
//! identical whether the solver runs in-process or over the network.

use async_trait::async_trait;
use tankovault_solver::{ChallengeSolver, SolveError, SolveOutcome, SolveRequest};
use std::time::Duration;

/// Client for the `challenge-solver` service `POST /v1/solve` endpoint.
pub struct HttpChallengeSolver {
    client: reqwest::Client,
    endpoint: String,
}

impl HttpChallengeSolver {
    /// Build a client for `endpoint` (e.g. `http://challenge-solver:8080`) with an
    /// overall solve timeout.
    #[must_use]
    pub fn new(endpoint: impl Into<String>, timeout: Duration) -> Self {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .expect("reqwest client builds with default TLS");
        Self {
            client,
            endpoint: endpoint.into(),
        }
    }
}

#[async_trait]
impl ChallengeSolver for HttpChallengeSolver {
    async fn solve(&self, req: SolveRequest) -> Result<SolveOutcome, SolveError> {
        let url = format!("{}/v1/solve", self.endpoint.trim_end_matches('/'));
        let resp = self
            .client
            .post(&url)
            .json(&req)
            .send()
            .await
            .map_err(|e| {
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
