//! The `POST /v1/solve` contract, defined once.
//!
//! Two services expose this endpoint — `challenge-solver` over `FlareSolverrSolver`, and
//! `render` over its `ChromiumSolver` — and `tankovault_fetch::HttpChallengeSolver` talks to
//! both, assuming they are interchangeable. That assumption was held up by nothing: the
//! handler was copy-pasted, so the status, the metric label and the error body could drift
//! apart with no compile error and no test, while the code that depends on them being
//! identical sat in a third crate.
//!
//! Behind the `axum` feature so the crate stays usable by consumers that only want the
//! [`ChallengeSolver`] trait and the detection helpers.

use crate::types::{ChallengeSolver, SolveRequest};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use std::sync::Arc;
use tankovault_service::problem::Problem;

/// The router fragment both solver-hosting services merge.
///
/// The caller keeps ownership of everything else — its own middleware stack, its ops probes,
/// its readiness policy. This contributes exactly one route.
pub fn solver_router(solver: Arc<dyn ChallengeSolver>) -> Router {
    Router::new()
        .route("/v1/solve", post(solve))
        .with_state(solver)
}

/// Validate the target, solve it, and report the outcome.
///
/// The URL check is not optional and not the caller's business to remember: this endpoint
/// fetches a caller-supplied URL and hands back the body, which is an arbitrary-URL fetch
/// primitive for anything that can reach the port. Both copies of this handler previously
/// omitted it.
async fn solve(
    State(solver): State<Arc<dyn ChallengeSolver>>,
    Json(req): Json<SolveRequest>,
) -> Response {
    if let Err(e) = tankovault_domain::ssrf::validate_str(&req.url) {
        metrics::counter!("solve_attempts_total", "result" => "rejected").increment(1);
        tracing::warn!(url = %req.url, error = %e, "refused a solve target");
        // The reason names only the caller's own URL and the policy rule that refused it, which
        // is what makes a misconfigured provider debuggable.
        return Problem::new(
            StatusCode::FORBIDDEN,
            "refused_target",
            format!("refused target: {e}"),
        )
        .into_response();
    }

    let provider = req.provider.clone();
    match solver.solve(req).await {
        Ok(outcome) => {
            metrics::counter!("solve_attempts_total", "result" => "ok").increment(1);
            (StatusCode::OK, Json(outcome)).into_response()
        }
        Err(e) => {
            metrics::counter!("solve_attempts_total", "result" => "error").increment(1);
            tracing::warn!(%provider, error = %e, "solve failed");
            // The cause is in the log; the caller gets the one RFC 9457 shape every service
            // emits, so `tankovault_fetch::HttpChallengeSolver` parses one format (ARCH-12).
            Problem::bad_gateway().into_response()
        }
    }
}
