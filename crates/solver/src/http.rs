//! The `POST /v1/solve` contract, defined once so `challenge-solver` and `render` — both of
//! which expose it, and both of which `tankovault_fetch::HttpChallengeSolver` assumes are
//! interchangeable — cannot drift apart in status, metric label or error shape.
//!
//! Behind the `axum` feature so the crate stays usable by consumers that only want the
//! [`ChallengeSolver`] trait and the detection helpers.

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
/// cannot be spelled differently.
pub const SOLVE_PATH: &str = "/v1/solve";

/// The router fragment both solver-hosting services merge.
///
/// The caller keeps ownership of everything else — its own middleware stack, its ops probes,
/// its readiness policy. This contributes exactly one route.
pub fn solver_router(solver: Arc<dyn ChallengeSolver>) -> Router {
    Router::new()
        .route(SOLVE_PATH, post(solve))
        .with_state(solver)
}

/// The internal-auth entry for the route [`solver_router`] contributes.
///
/// Declared beside the router for the reason the router itself is shared: this endpoint fetches
/// a caller-supplied URL, so a service that mounts it and authorises a *different* path has
/// published an arbitrary-URL fetch primitive to anything that can reach the port.
#[must_use]
pub const fn solve_route(callers: &'static [&'static str]) -> InternalRoute {
    InternalRoute {
        method: Method::POST,
        path: SOLVE_PATH,
        callers,
    }
}

/// Validate the target, solve it, and report the outcome.
///
/// The URL check is not optional and not the caller's business to remember: this endpoint
/// fetches a caller-supplied URL, which is an arbitrary-URL fetch primitive for anything
/// that can reach the port.
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
            let transient = e.is_transient();
            metrics::counter!(
                "solve_attempts_total",
                "result" => if transient { "unavailable" } else { "error" },
            )
            .increment(1);
            tracing::warn!(%provider, error = %e, transient, "solve failed");
            // The cause is in the log; the caller gets the one RFC 9457 shape every service
            // emits, so `tankovault_fetch::HttpChallengeSolver` only ever parses one format.
            //
            // The *status* is the only part of that shape the caller can act on, so it has to
            // carry the one distinction that changes what it does, and it has to survive an
            // intermediary. `5xx` means this tier could not serve the solve and the same request
            // is worth repeating; `4xx` means the exchange happened and produced no session,
            // which repeating only pays for again. The unprocessable status is deliberate rather
            // than `502`: a gateway in front of this service emits `502`/`504` of its own, and
            // those are unavailability, so the terminal answer must not share their range.
            // Answering `502` to both is what made a solver outage indistinguishable from nine
            // providers turning hostile.
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
