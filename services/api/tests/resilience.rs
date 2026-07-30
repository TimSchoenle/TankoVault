//! Production-readiness resilience & probe checks.
//!
//! Confirms the operational surface the orchestrator and the degradation contract depend on:
//! the liveness/readiness probes answer through the assembled router, and a dependency-down
//! path (NATS unreachable) degrades the live stream to `503` instead of failing the whole edge.
//!
//! Opt-in: gated behind the `integration` feature because they require Docker.
#![cfg(feature = "integration")]

use axum::http::StatusCode;
use tankovault_api_test_support::TestApp;
use tankovault_domain::AccountStatus;

#[tokio::test]
async fn liveness_and_readiness_probes_answer() {
    let app = TestApp::spawn().await;

    // Liveness never consults a dependency: it says only that the process is scheduling.
    let (status, _) = app.call("GET", "/health", None, None).await;
    assert_eq!(status, StatusCode::OK, "/health must report liveness");

    // Readiness answers through the same router the orchestrator scrapes.
    let (status, _) = app.call("GET", "/ready", None, None).await;
    assert!(
        status.is_success(),
        "/ready must report readiness, got {status}"
    );
}

#[tokio::test]
async fn the_live_stream_degrades_to_503_when_the_bus_is_unreachable() {
    // The harness wires no NATS bus (`bus: None`), standing in for an unreachable broker.
    let app = TestApp::spawn().await;
    let user = app.seed_user("streamer", &[], AccountStatus::Active).await;

    // `EventSource` cannot set an Authorization header, so the stream authenticates via a query
    // parameter — a single-use ticket rather than the access token it used to carry (SEC-8). The
    // ticket is genuinely valid, which is what proves the 503 is a *degradation* and not an auth
    // failure: a rejected credential answers 401, so the two outcomes are distinguishable.
    let ticket = app.stream_ticket(user).await;
    let (status, _) = app
        .call("GET", &format!("/v1/me/stream?ticket={ticket}"), None, None)
        .await;

    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "with the bus down the live stream must degrade to 503, not error the edge"
    );
}
