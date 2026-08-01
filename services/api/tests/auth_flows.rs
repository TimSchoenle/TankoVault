//! End-to-end tests for refresh-token rotation and reuse-detection family revocation, run with
//! no mailer so registration activates immediately. Gated behind the `integration` feature;
//! requires Docker.
#![cfg(feature = "integration")]

use axum::body::Body;
use axum::http::{Request, Response, StatusCode, header};
use serde_json::json;
use tankovault_api_test_support::TestApp;

/// The `__Host-` prefix makes `Secure`, `Path=/` and no `Domain` browser-enforced, matching
/// production.
const REFRESH_COOKIE: &str = "__Host-refresh_token";

/// Age every already-revoked token past `auth::session::ROTATION_GRACE` (60 s), so a replay
/// exercises reuse detection rather than the grace-window recovery path. Skipping this silently
/// turns a reuse test into a grace test instead.
const AGE_REVOCATIONS_PAST_GRACE: &str = "UPDATE refresh_tokens \
     SET revoked_at = now() - interval '5 minutes' WHERE revoked_at IS NOT NULL";

/// Build a POST request, optionally carrying a refresh cookie and/or a JSON body.
fn post(
    path: &str,
    refresh_cookie: Option<&str>,
    body: Option<serde_json::Value>,
) -> Request<Body> {
    let mut builder = Request::builder().method("POST").uri(path);
    if let Some(cookie) = refresh_cookie {
        builder = builder.header(header::COOKIE, format!("{REFRESH_COOKIE}={cookie}"));
    }
    match body {
        Some(json) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&json).expect("serialize")))
            .expect("build request"),
        None => builder.body(Body::empty()).expect("build request"),
    }
}

/// The value of the refresh cookie a response set, if any.
fn refresh_cookie(resp: &Response<Body>) -> Option<String> {
    for value in resp.headers().get_all(header::SET_COOKIE) {
        let Ok(text) = value.to_str() else { continue };
        if let Some(rest) = text.strip_prefix(&format!("{REFRESH_COOKIE}=")) {
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

    // Put the rotation out of reach of the grace window; see `AGE_REVOCATIONS_PAST_GRACE`.
    app.db.execute(AGE_REVOCATIONS_PAST_GRACE).await;

    let replayed = app
        .request(post("/v1/auth/refresh", Some(&first), None))
        .await;
    assert_eq!(
        replayed.status(),
        StatusCode::UNAUTHORIZED,
        "replaying a rotated token must be rejected"
    );

    // The second (previously valid) token is dead too: reuse revokes the whole family.
    let after_reuse = app
        .request(post("/v1/auth/refresh", Some(&second), None))
        .await;
    assert_eq!(
        after_reuse.status(),
        StatusCode::UNAUTHORIZED,
        "the whole family must be revoked once reuse is detected"
    );

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

/// A rotation raced by a second request must recover the session, not end it.
///
/// # The bug this pins
///
/// Reuse detection treated any revoked token as theft; two requests racing a rotation (or one
/// retrying a dropped response) both present the pre-rotation token, and `revoke_family` ended
/// the whole session instead of recovering.
#[tokio::test]
async fn a_raced_rotation_is_recovered_rather_than_ending_the_session() {
    let app = TestApp::spawn().await;

    let registered = app
        .request(post(
            "/v1/auth/register",
            None,
            Some(json!({
                "email": "juniper@example.test",
                "username": "juniper",
                "password": "correct horse battery",
            })),
        ))
        .await;
    assert_eq!(registered.status(), StatusCode::OK);
    let first = refresh_cookie(&registered).expect("registration issues a refresh cookie");

    let rotated = app
        .request(post("/v1/auth/refresh", Some(&first), None))
        .await;
    assert_eq!(rotated.status(), StatusCode::OK);
    let second = refresh_cookie(&rotated).expect("rotation issues a fresh refresh cookie");

    // No `AGE_REVOCATIONS` call: being inside the window is the whole point of this test.
    let raced = app
        .request(post("/v1/auth/refresh", Some(&first), None))
        .await;
    assert_eq!(
        raced.status(),
        StatusCode::OK,
        "a token replayed inside the rotation grace window is a raced rotation, not theft"
    );
    let third = refresh_cookie(&raced).expect("a recovered race still issues a fresh cookie");
    assert_ne!(third, first, "recovery must not hand back the spent token");
    assert_ne!(third, second, "recovery must issue its own token");

    // Before the reuse probe below, which would add a denial of its own.
    assert!(
        !app.audit
            .denials()
            .iter()
            .any(|e| e.detail["reason"] == "token_reuse_detected"),
        "a raced rotation must not be recorded as theft"
    );
    assert!(
        app.audit
            .events()
            .iter()
            .any(|e| e.action == "auth.refresh" && e.detail["reason"] == "rotation_race_recovered"),
        "the recovery must be audited under its own reason, so an operator can tell the two \
         apart in the log"
    );

    // Ageing the revocations first stops this replay being read as a second race.
    app.db.execute(AGE_REVOCATIONS_PAST_GRACE).await;
    let superseded = app
        .request(post("/v1/auth/refresh", Some(&second), None))
        .await;
    assert_eq!(
        superseded.status(),
        StatusCode::UNAUTHORIZED,
        "recovery must collapse the family, leaving only the token it just issued"
    );
}

/// The grace window requires a *live* family, not just a recent revocation.
///
/// A time bound alone would let a deliberately shut-down lineage be re-opened by a token
/// captured just before the shutdown — logout, then replay an older cookie, hands the session
/// back. This is the liveness half of the test, which a timeout-only "simplification" would drop.
#[tokio::test]
async fn a_replay_into_a_dead_family_is_reuse_even_inside_the_grace_window() {
    let app = TestApp::spawn().await;

    let registered = app
        .request(post(
            "/v1/auth/register",
            None,
            Some(json!({
                "email": "wren@example.test",
                "username": "wren",
                "password": "correct horse battery",
            })),
        ))
        .await;
    assert_eq!(registered.status(), StatusCode::OK);
    let first = refresh_cookie(&registered).expect("registration issues a refresh cookie");

    let rotated = app
        .request(post("/v1/auth/refresh", Some(&first), None))
        .await;
    assert_eq!(rotated.status(), StatusCode::OK);
    let second = refresh_cookie(&rotated).expect("rotation issues a fresh refresh cookie");

    let logged_out = app
        .request(post("/v1/auth/logout", Some(&second), None))
        .await;
    assert_eq!(logged_out.status(), StatusCode::OK);

    // Well inside the grace window: only the liveness check can refuse this.
    let replayed = app
        .request(post("/v1/auth/refresh", Some(&first), None))
        .await;
    assert_eq!(
        replayed.status(),
        StatusCode::UNAUTHORIZED,
        "a shut-down lineage must never re-open, however recent the revocation"
    );
    assert!(
        app.audit
            .denials()
            .iter()
            .any(|e| e.action == "auth.refresh" && e.detail["reason"] == "token_reuse_detected"),
        "a replay into a dead family is reuse and must be audited as such"
    );
}

#[tokio::test]
async fn refresh_without_a_cookie_is_unauthorized() {
    let app = TestApp::spawn().await;
    let resp = app.request(post("/v1/auth/refresh", None, None)).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}
