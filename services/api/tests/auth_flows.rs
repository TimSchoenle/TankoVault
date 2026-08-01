//! End-to-end auth-flow integration tests.
//!
//! These drive the real credential endpoints to prove the security-critical session lifecycle:
//! refresh-token rotation, reuse-detection family revocation, and the narrow grace window that
//! separates the two — a revoked token is the signature of theft *and* of a rotation an honest
//! client never took delivery of, so all three branches are pinned here. Because the harness
//! configures
//! no SMTP relay, registration activates the account immediately and issues a session (see
//! `auth::register`), so the flow can be exercised without an email round trip.
//!
//! Opt-in: gated behind the `integration` feature because they require Docker.
#![cfg(feature = "integration")]

use axum::body::Body;
use axum::http::{Request, Response, StatusCode, header};
use serde_json::json;
use tankovault_api_test_support::TestApp;

/// The cookie name a deployment that marks cookies `Secure` issues — which the harness now does
/// by default, matching production (SEC-7). The `__Host-` prefix makes `Secure`, `Path=/` and the
/// absence of `Domain` browser-enforced instead of merely configured.
const REFRESH_COOKIE: &str = "__Host-refresh_token";

/// Age every already-revoked token well past `auth::session::ROTATION_GRACE` (60 s).
///
/// Reuse detection has a *time-bounded* exemption: a token replayed within the grace window of
/// its own rotation, while its family still holds a live token, is an interrupted or raced
/// rotation by an honest client, not theft. A test that rotates and immediately replays is
/// therefore exercising the **grace** path, whatever its name says. Any test that means to
/// exercise reuse detection has to run this first, or it silently stops testing reuse — which
/// is exactly what would have happened to
/// [`refresh_rotates_the_token_and_reuse_revokes_the_family`] when the window was introduced.
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

    // Put the rotation out of reach of the grace window. Without this the replay below lands
    // inside it and is (correctly) recovered as a race, so every assertion that follows would
    // pass for the wrong reason or fail for a reason that has nothing to do with reuse. See
    // `AGE_REVOCATIONS_PAST_GRACE`.
    app.db.execute(AGE_REVOCATIONS_PAST_GRACE).await;

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

/// A rotation raced by a second request must recover the session, not end it.
///
/// # The bug this pins
///
/// Reuse detection used to treat *any* revoked token as theft, which meant it fired on the
/// commonest non-attack in the system. Rotation is atomic on the server and not from the
/// client's side: the old token is revoked and the new one issued together, but the client only
/// holds the new value once the response reaches it. Two tabs share one cookie jar and one
/// renewal timer schedule, so they fire together and the second request carries a cookie the
/// first has already rotated away — and a dropped response does the same thing to a single tab,
/// which then retries within seconds using the only value it has.
///
/// The consequence was not a failed request. `revoke_family` ends the *session*, so a 30-day
/// login died seconds after an API restart, with `token_reuse_detected` in the audit log and no
/// attacker involved. Observed in production with a **1.35 s** gap between the two requests.
///
/// So: the replay must be served, the family must collapse to exactly one live token, and the
/// event must be audited as itself rather than as theft.
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

    // The request that wins the race rotates normally.
    let rotated = app
        .request(post("/v1/auth/refresh", Some(&first), None))
        .await;
    assert_eq!(rotated.status(), StatusCode::OK);
    let second = refresh_cookie(&rotated).expect("rotation issues a fresh refresh cookie");

    // The request that lost it arrives holding the pre-rotation cookie. No `AGE_REVOCATIONS`
    // call here — being *inside* the window is the whole point of this test.
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

    // Audited as what it is. These assertions come before the reuse probe below, which would
    // add a denial of its own.
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

    // The family collapsed: the successor nobody took delivery of died with the recovery, so
    // the lineage carries exactly one live token rather than two. Ageing the revocations first
    // is what stops *this* replay being read as a second race.
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
/// The time bound alone would let a lineage that has already been deliberately shut down be
/// re-opened by a token captured just before the shutdown — logging out and then replaying an
/// older cookie a moment later would hand the session back. Liveness is the second half of the
/// test for that reason, and it is the half a "simplification" to a pure timeout would drop.
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

    // Logout revokes the whole family, so nothing in the lineage is live any more.
    let logged_out = app
        .request(post("/v1/auth/logout", Some(&second), None))
        .await;
    assert_eq!(logged_out.status(), StatusCode::OK);

    // Well inside the grace window — only the liveness half of the test can refuse this.
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
