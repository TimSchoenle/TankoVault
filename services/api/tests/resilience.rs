//! Production-readiness resilience & probe checks.
//!
//! Confirms the operational surface the orchestrator and the degradation contract depend on:
//! the liveness/readiness probes answer through the assembled router, and a dependency-down
//! path (NATS unreachable) degrades the live stream to `503` instead of failing the whole edge.
//!
//! Opt-in: gated behind the `integration` feature because they require Docker.
#![cfg(feature = "integration")]

use axum::http::StatusCode;
use tankovault_domain::AccountStatus;
use tankovault_test_support::TestApp;

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
    // parameter; the token is a valid access token, proving the 503 is a *degradation*, not an
    // auth failure.
    let raw_token = app
        .bearer(user)
        .strip_prefix("Bearer ")
        .expect("bearer prefix")
        .to_owned();
    let (status, _) = app
        .call(
            "GET",
            &format!("/v1/me/stream?access_token={raw_token}"),
            None,
            None,
        )
        .await;

    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "with the bus down the live stream must degrade to 503, not error the edge"
    );
}
