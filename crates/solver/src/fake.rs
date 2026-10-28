//! Provides a deterministic in-process solver.
//!
//! It is the fixture for the test that the fetch pipeline is solver-agnostic, and a safe default
//! when no real solver is configured: it returns an empty session, so callers degrade instead
//! of failing.

use crate::types::{ChallengeSolver, SolveError, SolveOutcome, SolveRequest};
use async_trait::async_trait;

/// Returns a fixed [`SolveOutcome`] for every request.
pub struct StaticSolver {
    outcome: SolveOutcome,
}

impl StaticSolver {
    /// Builds a solver that always yields `outcome`.
    #[must_use]
    pub fn new(outcome: SolveOutcome) -> Self {
        Self { outcome }
    }

    /// Builds a solver that returns an empty session with the given user agent.
    #[must_use]
    pub fn cleared(user_agent: impl Into<String>, ttl_secs: u64) -> Self {
        Self {
            outcome: SolveOutcome {
                cookies: Vec::new(),
                user_agent: user_agent.into(),
                html: None,
                status: None,
                headers: Vec::new(),
                ttl_secs,
            },
        }
    }
}

#[async_trait]
impl ChallengeSolver for StaticSolver {
    async fn solve(&self, _req: SolveRequest) -> Result<SolveOutcome, SolveError> {
        Ok(self.outcome.clone())
    }

    fn backend_name(&self) -> &'static str {
        "static"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ChallengeKind;

    // The fetch pipeline depends only on `dyn ChallengeSolver`, so any back-end drops in
    // without a pipeline change.
    async fn drive(solver: &dyn ChallengeSolver, url: &str) -> SolveOutcome {
        solver
            .solve(SolveRequest {
                url: url.to_owned(),
                provider: "test".to_owned(),
                kind: Some(ChallengeKind::CloudflareJs),
            })
            .await
            .expect("static solver never fails")
    }

    #[tokio::test]
    async fn swappable_backend_returns_session() {
        let solver = StaticSolver::new(SolveOutcome {
            cookies: vec![("cf_clearance".into(), "abc".into())],
            user_agent: "UA/1.0".into(),
            html: Some("<html>solved</html>".into()),
            status: Some(200),
            headers: Vec::new(),
            ttl_secs: 900,
        });
        let out = drive(&solver, "https://example.test/manga").await;
        assert_eq!(out.user_agent, "UA/1.0");
        assert_eq!(out.cookies[0].0, "cf_clearance");
        assert_eq!(solver.backend_name(), "static");
    }
}
