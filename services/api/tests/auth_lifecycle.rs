//! The credential lifecycle that only exists when a mailer is configured.
//!
//! # What this covers that nothing did
//!
//! `auth::register` forks on `state.mailer.is_enabled()`. Until [`RecordingMailer`] existed,
//! `TestApp` wired the disabled default, so **every test took the "no mailer, activate
//! immediately" branch** — the email-confirmation half of registration, the confirmation
//! resend, and the whole password-reset flow had never been executed by any test at all.
//! These drive the other side of that fork end to end, the way a user does: read the link out
//! of the delivered message, then present it.
//!
//! `auth_flows.rs` remains the specification for refresh-token rotation and reuse detection
//! and is deliberately not duplicated here.
//!
//! Opt-in: gated behind the `integration` feature because it requires Docker.
#![cfg(feature = "integration")]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, Response, StatusCode, header};
use serde_json::{Value, json};
use tankovault_api_test_support::{TestApp, TestConfig};
use tankovault_test_support::RecordingMailer;

/// Stand up a router whose mailer is configured, plus the recorder to read messages from.
///
/// Rate limiting is off: `/v1/auth` draws from the tightest budget in the service (10/min,
/// burst 5) and a single lifecycle here — register, confirm, reset, sign in — spends more than
/// that. Leaving it on would report a throttle as an authentication failure.
async fn app_with_mail() -> (TestApp, Arc<RecordingMailer>) {
    let mailer = Arc::new(RecordingMailer::enabled());
    let app = TestApp::spawn_with(
        TestConfig::new()
            .with_mailer(mailer.clone())
            .without_rate_limiting(),
    )
    .await;
    (app, mailer)
}

/// A router with no mailer — the branch where registration activates immediately.
async fn app_without_mail() -> TestApp {
    TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await
}

fn post(path: &str, cookie: Option<&str>, body: Option<Value>) -> Request<Body> {
    let mut builder = Request::builder().method("POST").uri(path);
    if let Some(cookie) = cookie {
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

/// The raw `Set-Cookie` line for `refresh_token`, whatever its value.
fn refresh_set_cookie(resp: &Response<Body>) -> Option<String> {
    resp.headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|t| t.starts_with("refresh_token="))
        .map(str::to_owned)
}

/// The refresh token a response issued, or `None` when it issued none (or cleared it).
fn refresh_token(resp: &Response<Body>) -> Option<String> {
    let line = refresh_set_cookie(resp)?;
    let value = line
        .strip_prefix("refresh_token=")?
        .split(';')
        .next()
        .unwrap_or_default();
    (!value.is_empty()).then(|| value.to_owned())
}

async fn json_body(resp: Response<Body>) -> Value {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("read body");
    if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    }
}

/// Wait for the next delivered message whose link points at `route`, and return its `?token=`.
///
/// Messages are pulled until one matches rather than assuming the next one does: confirming an
/// address also queues the deferred welcome email, so the reset link is not always the message
/// that arrives next.
async fn next_link_token(mailer: &RecordingMailer, route: &str) -> String {
    for _ in 0..8 {
        let message = mailer.next_message().await;
        let Some(link) = RecordingMailer::first_link(&message.text) else {
            continue;
        };
        if !link.contains(route) {
            continue;
        }
        return link
            .split("token=")
            .nth(1)
            .expect("the link carries a token")
            .to_owned();
    }
    panic!("no message carrying a {route} link was delivered");
}

/// Register `username`, returning the response so the caller can assert on its shape.
async fn register(app: &TestApp, username: &str) -> Response<Body> {
    app.request(post(
        "/v1/auth/register",
        None,
        Some(json!({
            "email": format!("{username}@example.test"),
            "username": username,
            "password": "correct horse battery",
        })),
    ))
    .await
}

#[tokio::test]
async fn registering_with_a_mailer_configured_requires_confirmation_and_issues_no_session() {
    let (app, mailer) = app_with_mail().await;

    let resp = register(&app, "unconfirmed").await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        refresh_token(&resp).is_none(),
        "an unconfirmed registration must not issue a refresh cookie"
    );

    let body = json_body(resp).await;
    assert_eq!(
        body["verification_required"], true,
        "the client is told to wait for the confirmation link"
    );
    assert!(
        body.get("access_token").is_none(),
        "an unconfirmed registration must not issue an access token, got {body}"
    );

    // The confirmation must actually be sent, and to the address that registered. A branch
    // that sets `verification_required` without delivering anything strands the account.
    let message = mailer.next_message().await;
    assert_eq!(message.to, vec!["unconfirmed@example.test".to_owned()]);
    assert!(
        RecordingMailer::first_link(&message.text).is_some(),
        "the confirmation message must carry a link, got {:?}",
        message.text
    );
}

#[tokio::test]
async fn an_unconfirmed_account_cannot_log_in_and_is_told_why() {
    let (app, _mailer) = app_with_mail().await;
    register(&app, "waiting").await;

    let resp = app
        .request(post(
            "/v1/auth/login",
            None,
            Some(json!({ "login": "waiting", "password": "correct horse battery" })),
        ))
        .await;

    // 403 rather than 401, and named: the client offers "resend the link" off this, so
    // collapsing it into a generic bad-credentials 401 would leave the user with no way out.
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert_eq!(json_body(resp).await["title"], "email_not_verified");
}

#[tokio::test]
async fn confirming_the_emailed_link_verifies_the_address_and_signs_the_user_in() {
    let (app, mailer) = app_with_mail().await;
    register(&app, "confirmer").await;
    let token = next_link_token(&mailer, "/verify-email").await;

    let resp = app
        .request(post(
            "/v1/auth/verify-email",
            None,
            Some(json!({ "token": token })),
        ))
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        refresh_token(&resp).is_some(),
        "confirming the link must land the user in the app with a session"
    );
    assert!(
        json_body(resp).await["access_token"].is_string(),
        "confirming the link must issue an access token"
    );

    // And the account is now usable by the ordinary route.
    let resp = app
        .request(post(
            "/v1/auth/login",
            None,
            Some(json!({ "login": "confirmer", "password": "correct horse battery" })),
        ))
        .await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "a confirmed account signs in"
    );
}

#[tokio::test]
async fn a_confirmation_token_is_single_use() {
    // The `used_at` flip is what closes the race between two concurrent confirmations. A
    // replayable link would also mean a link leaked from an inbox stays usable forever.
    let (app, mailer) = app_with_mail().await;
    register(&app, "replayer").await;
    let token = next_link_token(&mailer, "/verify-email").await;

    let first = app
        .request(post(
            "/v1/auth/verify-email",
            None,
            Some(json!({ "token": token.clone() })),
        ))
        .await;
    assert_eq!(first.status(), StatusCode::OK);

    let replay = app
        .request(post(
            "/v1/auth/verify-email",
            None,
            Some(json!({ "token": token })),
        ))
        .await;
    assert_eq!(replay.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_confirmation_token_past_its_ttl_is_refused() {
    // VERIFY_TOKEN_TTL is 24 hours. Nothing can advance the clock from a test, so the row is
    // aged instead — which exercises the same `expires_at <= now()` comparison the handler
    // makes, and would catch that comparison being dropped or inverted.
    let (app, mailer) = app_with_mail().await;
    register(&app, "latecomer").await;
    let token = next_link_token(&mailer, "/verify-email").await;

    app.db
        .execute("UPDATE email_verification_tokens SET expires_at = now() - interval '1 hour'")
        .await;

    let resp = app
        .request(post(
            "/v1/auth/verify-email",
            None,
            Some(json!({ "token": token })),
        ))
        .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn resending_the_confirmation_issues_a_second_working_link() {
    let (app, mailer) = app_with_mail().await;
    register(&app, "resender").await;
    let first = next_link_token(&mailer, "/verify-email").await;

    let resp = app
        .request(post(
            "/v1/auth/verify-email/resend",
            None,
            Some(json!({ "email": "resender@example.test" })),
        ))
        .await;
    assert_eq!(resp.status(), StatusCode::ACCEPTED);

    let second = next_link_token(&mailer, "/verify-email").await;
    assert_ne!(first, second, "a resend must mint a fresh token");

    // Both links belong to the same account and neither has been consumed, so the newest one
    // must work — a resend that invalidated nothing but also confirmed nothing is a dead end.
    let resp = app
        .request(post(
            "/v1/auth/verify-email",
            None,
            Some(json!({ "token": second })),
        ))
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn resending_for_an_unknown_address_reveals_nothing() {
    // Both branches must be indistinguishable to the caller, or the endpoint becomes an
    // account-existence oracle that needs no credentials at all.
    let (app, _mailer) = app_with_mail().await;
    let resp = app
        .request(post(
            "/v1/auth/verify-email/resend",
            None,
            Some(json!({ "email": "nobody@example.test" })),
        ))
        .await;
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
}

#[tokio::test]
async fn a_reset_link_changes_the_password_and_kills_every_live_session() {
    let (app, mailer) = app_with_mail().await;
    register(&app, "forgetful").await;
    let confirmation = next_link_token(&mailer, "/verify-email").await;
    let signed_in = app
        .request(post(
            "/v1/auth/verify-email",
            None,
            Some(json!({ "token": confirmation })),
        ))
        .await;
    let live_session = refresh_token(&signed_in).expect("confirming issues a session");

    let resp = app
        .request(post(
            "/v1/auth/password/forgot",
            None,
            Some(json!({ "email": "forgetful@example.test" })),
        ))
        .await;
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    let reset = next_link_token(&mailer, "/reset-password").await;

    let resp = app
        .request(post(
            "/v1/auth/password/reset",
            None,
            Some(json!({ "token": reset, "new_password": "a whole new passphrase" })),
        ))
        .await;
    assert_eq!(resp.status(), StatusCode::OK);

    // The point of the reset is that a stolen credential stops working. A refresh cookie
    // minted under the old password must die with it, or the attacker keeps their session.
    let resp = app
        .request(post("/v1/auth/refresh", Some(&live_session), None))
        .await;
    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "a reset must revoke sessions held under the old password"
    );

    let resp = app
        .request(post(
            "/v1/auth/login",
            None,
            Some(json!({ "login": "forgetful", "password": "a whole new passphrase" })),
        ))
        .await;
    assert_eq!(resp.status(), StatusCode::OK, "the new password works");

    let resp = app
        .request(post(
            "/v1/auth/login",
            None,
            Some(json!({ "login": "forgetful", "password": "correct horse battery" })),
        ))
        .await;
    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "the old password must not still work"
    );
}

#[tokio::test]
async fn a_reset_token_is_single_use() {
    let (app, mailer) = app_with_mail().await;
    register(&app, "doubledipper").await;

    app.request(post(
        "/v1/auth/password/forgot",
        None,
        Some(json!({ "email": "doubledipper@example.test" })),
    ))
    .await;
    let token = next_link_token(&mailer, "/reset-password").await;

    let first = app
        .request(post(
            "/v1/auth/password/reset",
            None,
            Some(json!({ "token": token.clone(), "new_password": "first replacement" })),
        ))
        .await;
    assert_eq!(first.status(), StatusCode::OK);

    let replay = app
        .request(post(
            "/v1/auth/password/reset",
            None,
            Some(json!({ "token": token, "new_password": "second replacement" })),
        ))
        .await;
    assert_eq!(
        replay.status(),
        StatusCode::BAD_REQUEST,
        "a consumed reset token must not set the password a second time"
    );
}

#[tokio::test]
async fn a_reset_token_past_its_ttl_is_refused() {
    // RESET_TOKEN_TTL is one hour, and the short window is the whole point: it bounds the
    // blast radius of a leaked inbox. Age the row rather than the clock; see the confirmation
    // TTL test for why.
    let (app, mailer) = app_with_mail().await;
    register(&app, "sluggish").await;

    app.request(post(
        "/v1/auth/password/forgot",
        None,
        Some(json!({ "email": "sluggish@example.test" })),
    ))
    .await;
    let token = next_link_token(&mailer, "/reset-password").await;

    app.db
        .execute("UPDATE password_reset_tokens SET expires_at = now() - interval '1 minute'")
        .await;

    let resp = app
        .request(post(
            "/v1/auth/password/reset",
            None,
            Some(json!({ "token": token, "new_password": "too late for this" })),
        ))
        .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_reset_request_for_an_unknown_address_reveals_nothing_and_sends_nothing() {
    let (app, mailer) = app_with_mail().await;
    let resp = app
        .request(post(
            "/v1/auth/password/forgot",
            None,
            Some(json!({ "email": "stranger@example.test" })),
        ))
        .await;
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    assert!(
        mailer.sent().is_empty(),
        "nothing may be sent for an address with no account"
    );
}

#[tokio::test]
async fn the_refresh_cookie_is_httponly_strictly_scoped_and_not_readable_by_script() {
    // These attributes are the whole containment story for the refresh token, and a
    // regression that widened `Path` to `/` would send it to every route including the ones
    // that echo request state. Nothing else in the suite would notice.
    let app = app_without_mail().await;
    let resp = register(&app, "cookiewatcher").await;
    let line = refresh_set_cookie(&resp).expect("registration sets the refresh cookie");

    assert!(line.contains("HttpOnly"), "not HttpOnly: {line}");
    assert!(
        line.contains("SameSite=Strict"),
        "not SameSite=Strict: {line}"
    );
    assert!(
        line.contains("Path=/v1/auth"),
        "the cookie must be scoped to the credential routes, got {line}"
    );
}

#[tokio::test]
async fn logging_out_clears_the_cookie_and_revokes_the_whole_family() {
    let app = app_without_mail().await;
    let session = refresh_token(&register(&app, "departing").await).expect("a session");

    let resp = app
        .request(post("/v1/auth/logout", Some(&session), None))
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let cleared = refresh_set_cookie(&resp).expect("logout sets a clearing cookie");
    assert!(
        cleared.starts_with("refresh_token=;") || cleared.contains("refresh_token=;"),
        "logout must clear the cookie value, got {cleared}"
    );

    // Clearing the browser's copy is cosmetic; revoking the family server-side is what makes
    // logging out mean anything to someone who kept the value.
    let resp = app
        .request(post("/v1/auth/refresh", Some(&session), None))
        .await;
    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "a token presented after logout must be dead server-side"
    );
}

#[tokio::test]
async fn a_suspended_account_is_refused_at_the_door_and_told_so() {
    let app = app_without_mail().await;
    register(&app, "banished").await;
    app.db
        .execute("UPDATE users SET status = 'suspended' WHERE username = 'banished'")
        .await;

    // Suspension must be reported *as* suspension, with correct credentials, rather than as a
    // bad password: a user told "wrong password" retries forever against an account that will
    // never work, and support cannot see why.
    let resp = app
        .request(post(
            "/v1/auth/login",
            None,
            Some(json!({ "login": "banished", "password": "correct horse battery" })),
        ))
        .await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert_eq!(json_body(resp).await["title"], "account_suspended");
}

#[tokio::test]
async fn an_unknown_identifier_is_indistinguishable_from_a_wrong_password() {
    // SEC-10: the unknown-identifier branch verifies a dummy argon2 hash so both branches
    // cost the same. This pins the *observable* half of that contract — same status, same
    // body — which is what an attacker actually reads. The timing half is pinned by
    // `auth.rs::the_dummy_hash_is_a_real_argon2id_hash_with_the_live_parameters`.
    let app = app_without_mail().await;
    register(&app, "genuine").await;

    let unknown = app
        .request(post(
            "/v1/auth/login",
            None,
            Some(json!({ "login": "ghost", "password": "correct horse battery" })),
        ))
        .await;
    let wrong = app
        .request(post(
            "/v1/auth/login",
            None,
            Some(json!({ "login": "genuine", "password": "not the password" })),
        ))
        .await;

    assert_eq!(unknown.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(json_body(unknown).await, json_body(wrong).await);
}
