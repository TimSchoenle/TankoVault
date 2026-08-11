//! `GET /v1/client`, over the real router.
//!
//! # What these pin
//!
//! **That the deployment names a ceiling even when the operator did not.** The client treats an
//! absent range as no range, which is the behaviour that existed before this endpoint — so a view
//! that omitted `max_version` would compile, deserialise and quietly do nothing at all.
//!
//! **That it answers without a token.** The updater runs from the moment the app starts, before
//! any session exists; behind auth it would fall back to no ceiling on every private deployment.
//!
//! Gated behind the `integration` feature because they require Docker.
#![cfg(feature = "integration")]

use axum::http::StatusCode;
use tankovault_api_test_support::{TestApp, TestConfig};
use tankovault_config::ClientConfig;

#[tokio::test]
async fn an_unconfigured_deployment_publishes_the_upstream_channel() {
    let app = TestApp::spawn_with(TestConfig::new()).await;

    let (status, body) = app.call("GET", "/v1/client", None, None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the updater runs before there is a session"
    );
    assert_eq!(body["release_repo"], "TimSchoenle/TankoVault");
    assert!(body["min_version"].is_null(), "{body}");
    // Filled in from the running service rather than left absent: the client reads an absent
    // ceiling as no ceiling, so an unset one has to resolve here or the range means nothing.
    assert!(
        body["max_version"].as_str().is_some_and(|v| !v.is_empty()),
        "{body}"
    );
}

/// A fork points its readers at its own installers, and pins the range it can talk to.
#[tokio::test]
async fn a_configured_deployment_publishes_its_own_channel() {
    let client = ClientConfig {
        release_repo: "example/mangabox".to_owned(),
        min_version: Some("1.5.0".to_owned()),
        max_version: Some("2.1.0".to_owned()),
    };
    let app = TestApp::spawn_with(TestConfig::new().with_client(client)).await;

    let (status, body) = app.call("GET", "/v1/client", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["release_repo"], "example/mangabox");
    assert_eq!(body["min_version"], "1.5.0");
    assert_eq!(body["max_version"], "2.1.0");
}

/// A deployment that names no repository publishes none, rather than publishing the upstream one
/// on its behalf — the client then stays on whichever repository it was built with, which for a
/// fork's client is the fork's.
#[tokio::test]
async fn a_blank_repository_is_published_as_absent() {
    let client = ClientConfig {
        release_repo: String::new(),
        ..ClientConfig::default()
    };
    let app = TestApp::spawn_with(TestConfig::new().with_client(client)).await;

    let (_, body) = app.call("GET", "/v1/client", None, None).await;
    assert!(body["release_repo"].is_null(), "{body}");
}
