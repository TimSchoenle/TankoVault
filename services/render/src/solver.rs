//! An alternate [`ChallengeSolver`] back-end that solves via a real headless browser instead
//! of `FlareSolverr`; same contract, so the fetch pipeline can't tell which is in play.

use std::sync::Arc;

use async_trait::async_trait;
use tankovault_solver::{ChallengeSolver, SolveError, SolveOutcome, SolveRequest};

use crate::browser::{BrowserManager, RenderOptions, RenderResult};

/// A [`ChallengeSolver`] that solves by rendering the page in a real browser.
pub(crate) struct ChromiumSolver {
    manager: Arc<BrowserManager>,
    ttl_secs: u64,
    challenge_wait_ms: u64,
}

impl ChromiumSolver {
    pub(crate) fn new(manager: Arc<BrowserManager>, ttl_secs: u64, challenge_wait_ms: u64) -> Self {
        Self {
            manager,
            ttl_secs,
            challenge_wait_ms,
        }
    }
}

/// Turns a rendered page into a reusable solver session; kept pure so it's testable
/// without a live Chrome.
pub(crate) fn render_result_into_outcome(result: RenderResult, ttl_secs: u64) -> SolveOutcome {
    SolveOutcome {
        cookies: result.cookies,
        user_agent: result.user_agent,
        html: if result.html.is_empty() {
            None
        } else {
            Some(result.html)
        },
        // No status reaches the fetch stack from this path; it must detect a throttle
        // notice by reading the page (`is_rate_limit_page`) rather than trusting a 200.
        status: None,
        headers: Vec::new(),
        ttl_secs,
    }
}

#[async_trait]
impl ChallengeSolver for ChromiumSolver {
    async fn solve(&self, req: SolveRequest) -> Result<SolveOutcome, SolveError> {
        let opts = RenderOptions {
            url: req.url,
            wait_selector: None,
            // Give the interstitial time to run its JS and set `cf_clearance`.
            wait_ms: self.challenge_wait_ms,
        };
        let result = self
            .manager
            .render(opts)
            .await
            .map_err(|e| SolveError::Transport(e.to_string()))?;
        Ok(render_result_into_outcome(result, self.ttl_secs))
    }

    fn backend_name(&self) -> &'static str {
        "chromium_render"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(html: &str) -> RenderResult {
        RenderResult {
            final_url: "https://example.test/".to_owned(),
            html: html.to_owned(),
            cookies: vec![("cf_clearance".to_owned(), "abc".to_owned())],
            user_agent: "Mozilla/5.0 TankoVault".to_owned(),
        }
    }

    #[test]
    fn outcome_carries_session_and_html() {
        let outcome = render_result_into_outcome(sample("<html>ok</html>"), 900);
        assert_eq!(outcome.ttl_secs, 900);
        assert_eq!(outcome.user_agent, "Mozilla/5.0 TankoVault");
        assert_eq!(outcome.cookies.len(), 1);
        assert_eq!(outcome.cookies[0].0, "cf_clearance");
        assert_eq!(outcome.html.as_deref(), Some("<html>ok</html>"));
    }

    #[test]
    fn empty_html_becomes_none() {
        let outcome = render_result_into_outcome(sample(""), 60);
        assert!(outcome.html.is_none());
        assert_eq!(outcome.ttl_secs, 60);
    }
}
