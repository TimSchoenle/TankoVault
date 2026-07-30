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
