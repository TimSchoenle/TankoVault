//! End-to-end auth-flow integration tests.
//!
//! These drive the real credential endpoints to prove the security-critical session lifecycle:
//! refresh-token rotation and reuse-detection family revocation. Because the harness configures
//! no SMTP relay, registration activates the account immediately and issues a session (see
//! `auth::register`), so the flow can be exercised without an email round trip.
//!
//! Opt-in: gated behind the `integration` feature because they require Docker.
#![cfg(feature = "integration")]

use axum::body::Body;
use axum::http::{Request, Response, StatusCode, header};
use serde_json::json;
use tankovault_test_support::TestApp;

/// Build a POST request, optionally carrying a refresh cookie and/or a JSON body.
fn post(
    path: &str,
    refresh_cookie: Option<&str>,
    body: Option<serde_json::Value>,
) -> Request<Body> {
    let mut builder = Request::builder().method("POST").uri(path);
    if let Some(cookie) = refresh_cookie {
        builder = builder.header(header::COOKIE, format!("refresh_token={cookie}"));
    }
    match body {
        Some(json) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&json).expect("serialize")))
            .expect("build request"),
        None => builder.body(Body::empty()).expect("build request"),
    }
}

/// The value of the `refresh_token` cookie a response set, if any.
fn refresh_cookie(resp: &Response<Body>) -> Option<String> {
    for value in resp.headers().get_all(header::SET_COOKIE) {
        let Ok(text) = value.to_str() else { continue };
        if let Some(rest) = text.strip_prefix("refresh_token=") {
            let token = rest.split(';').next().unwrap_or_default();
            // A cleared cookie (logout / reuse) has an empty value; treat that as "none".
            if !token.is_empty() {
                return Some(token.to_owned());
            }
        }
    }
    None
}

#[tokio::test]
async fn refresh_rotates_the_token_and_reuse_revokes_the_family() {
    let app = TestApp::spawn().await;

    // Registration with no configured mailer activates the account and issues a session,
    // setting the initial refresh cookie.
    let registered = app
        .request(post(
            "/v1/auth/register",
            None,
            Some(json!({
                "email": "aster@example.test",
                "username": "aster",
                "password": "correct horse battery",
            })),
        ))
        .await;
    assert_eq!(
        registered.status(),
        StatusCode::OK,
        "registration should succeed"
    );
    let first = refresh_cookie(&registered).expect("registration issues a refresh cookie");

    // Rotation: presenting the current token mints a *new* one and revokes the old.
    let rotated = app
        .request(post("/v1/auth/refresh", Some(&first), None))
        .await;
    assert_eq!(
        rotated.status(),
        StatusCode::OK,
        "a live refresh token should rotate"
    );
    let second = refresh_cookie(&rotated).expect("rotation issues a fresh refresh cookie");
    assert_ne!(first, second, "rotation must issue a different token");

    // Reuse: replaying the already-rotated first token is the highest-signal theft indicator.
    // It must be refused and must revoke the whole lineage.
    let replayed = app
        .request(post("/v1/auth/refresh", Some(&first), None))
        .await;
    assert_eq!(
        replayed.status(),
        StatusCode::UNAUTHORIZED,
        "replaying a rotated token must be rejected"
    );

    // Family revocation: the *second* (previously valid) token is now dead too, because the
    // reuse revoked its entire family — a stolen lineage cannot outlive detection.
    let after_reuse = app
        .request(post("/v1/auth/refresh", Some(&second), None))
        .await;
    assert_eq!(
        after_reuse.status(),
        StatusCode::UNAUTHORIZED,
        "the whole family must be revoked once reuse is detected"
    );

    // The reuse must be audited with its cause, so an operator learns a token was stolen.
    let reuse_events: Vec<_> = app
        .audit
        .denials()
        .into_iter()
        .filter(|e| e.action == "auth.refresh")
        .collect();
    assert!(
        reuse_events
            .iter()
            .any(|e| e.detail["reason"] == "token_reuse_detected"),
        "reuse detection must emit an audited denial naming the cause"
    );
}

#[tokio::test]
async fn refresh_without_a_cookie_is_unauthorized() {
    let app = TestApp::spawn().await;
    let resp = app.request(post("/v1/auth/refresh", None, None)).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}
