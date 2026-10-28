//! Defines the `POST /v1/solve` contract once.
//!
//! `challenge-solver` and `render` both expose it, and `tankovault_fetch::HttpChallengeSolver`
//! treats them as interchangeable, so neither may drift in status, metric label or error shape.
//! It sits behind the `axum` feature for consumers that want only the [`ChallengeSolver`] trait
//! and the detection helpers.

use crate::types::{ChallengeSolver, SolveRequest};
use axum::extract::State;
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use std::sync::Arc;
use tankovault_service::InternalRoute;
use tankovault_service::problem::Problem;

/// The one path [`solver_router`] mounts, named so the route and its authorisation entry
/// cannot differ.
pub const SOLVE_PATH: &str = "/v1/solve";

/// Returns the router fragment both solver-hosting services merge: exactly one route.
///
/// The caller owns everything else: its middleware stack, its ops probes, its readiness
/// policy.
pub fn solver_router(solver: Arc<dyn ChallengeSolver>) -> Router {
    Router::new()
        .route(SOLVE_PATH, post(solve))
        .with_state(solver)
}

/// Returns the internal-auth entry for the route [`solver_router`] contributes.
///
/// It is declared beside the router because this endpoint fetches a caller-supplied URL: a
/// service that mounts it and authorises a different path has published an arbitrary-URL fetch
/// primitive to anything that can reach the port.
#[must_use]
pub const fn solve_route(callers: &'static [&'static str]) -> InternalRoute {
    InternalRoute {
        method: Method::POST,
        path: SOLVE_PATH,
        callers,
    }
}

/// Validates the target, solves it, and reports the outcome.
///
/// The URL check lives here so no caller has to remember it: this endpoint fetches a
/// caller-supplied URL. It resolves the host, because the back-end resolves it again on its
/// own, and a literal-only check would let `http://redis:6379/` and `http://localhost/`
/// through.
async fn solve(
    State(solver): State<Arc<dyn ChallengeSolver>>,
    Json(req): Json<SolveRequest>,
) -> Response {
    if let Err(e) = tankovault_domain::ssrf::validate_unpinned(&req.url).await {
        metrics::counter!("solve_attempts_total", "result" => "rejected").increment(1);
        tracing::warn!(url = %req.url, error = %e, "refused a solve target");
        // The reason names only the caller's own URL and the rule that refused it, which keeps
        // a misconfigured provider debuggable.
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
            let transient = e.is_transient();
            metrics::counter!(
                "solve_attempts_total",
                "result" => if transient { "unavailable" } else { "error" },
            )
            .increment(1);
            tracing::warn!(%provider, error = %e, transient, "solve failed");
            // The cause is in the log; the caller gets the one RFC 9457 shape every service
            // emits, so `tankovault_fetch::HttpChallengeSolver` parses one format.
            //
            // The status is the part the caller acts on, so it carries the one distinction that
            // matters and must survive an intermediary. `5xx` means this tier could not serve
            // the solve and a repeat is worthwhile; `4xx` means the exchange produced no
            // session and a repeat only pays for it again. `422`, not `502`, because a gateway
            // in front of this service emits `502` and `504` of its own for unavailability.
            if transient {
                Problem::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "solver_unavailable",
                    "the challenge solver cannot serve requests right now; please try again",
                )
                .into_response()
            } else {
                Problem::new(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "challenge_unsolved",
                    "the challenge could not be solved for this target",
                )
                .into_response()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StaticSolver;

    async fn status_for(url: &str) -> StatusCode {
        let solver: Arc<dyn ChallengeSolver> = Arc::new(StaticSolver::cleared("ua", 60));
        let req = SolveRequest {
            url: url.to_owned(),
            provider: "p".to_owned(),
            kind: None,
        };
        solve(State(solver), Json(req)).await.status()
    }

    /// A hostname resolving to loopback is refused.
    ///
    /// The bug: checking only IP literals let a hostname naming loopback or a compose peer reach
    /// TRAWL, which then fetched it from inside the network.
    #[tokio::test]
    async fn refuses_a_hostname_that_resolves_to_loopback() {
        assert_eq!(
            status_for("http://localhost:6379/").await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(status_for("http://127.0.0.1/").await, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn accepts_a_public_ip_literal() {
        assert_eq!(status_for("http://1.1.1.1/").await, StatusCode::OK);
    }
}
