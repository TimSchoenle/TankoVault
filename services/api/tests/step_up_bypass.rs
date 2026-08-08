//! Ways an elevation could outlive what earned it.
//!
//! `mfa.rs` covers the prompt working: which routes demand a grant, which factors buy one, and
//! that one account cannot present another's. This file covers the other side — the four events
//! that are supposed to *end* an elevation, and the two values that must never be mistaken for
//! one.
//!
//! Why they need their own suite: a grant is a bearer credential with no per-request proof
//! behind it, so between issue and expiry the token *is* the elevation. Every bound on it is a
//! predicate in a `SELECT` or a `revoke_step_ups` call somebody remembered to make, and dropping
//! either leaves a system that passes every functional test — the prompt still opens, the right
//! code still works, the sensitive route still runs — while a captured grant stays good long
//! after the session, the password, or the factor that earned it is gone.
//!
//! Each test proves the elevation worked *first*, then that the event killed it. Without that
//! control a broken elevation would satisfy every assertion here by never having worked at all.
//!
//! Opt-in: gated behind the `integration` feature because they require Docker.
#![cfg(feature = "integration")]

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::json;
use tankovault_api_test_support::{TestApp, TestConfig};
use tankovault_db::repo::users::mfa::StepUpMethod;
use tankovault_domain::{AccountStatus, UserId};

/// The password every account here is registered with.
const PASSWORD: &str = "correct horse battery staple";

/// The cookie a deployment marking cookies `Secure` issues, which the harness does.
const REFRESH_COOKIE: &str = "__Host-refresh_token";

/// The canary. `GET /v1/me/export` is behind `Elevated`, takes no body, and answers `200` for an
/// account with nothing in it — so "did the elevation count" reads off the status alone.
const SENSITIVE: &str = "/v1/me/export";

/// Register through the real sign-up route, so the account has a password hash the login and
/// step-up paths can actually verify. `TestDb::seed_user` writes a placeholder.
async fn register(app: &TestApp, username: &str) -> UserId {
    register_with_session(app, username).await.0
}

/// [`register`], also returning the refresh cookie the new session was issued with.
///
/// Taken here rather than from a later sign-in on purpose: with no mailer configured
/// registration activates the account and issues the session immediately, whereas signing in
/// again *after* a factor is enrolled answers with a challenge and no cookie at all — which
/// would leave the sign-out test revoking nothing and passing for it.
async fn register_with_session(app: &TestApp, username: &str) -> (UserId, String) {
    let response = app
        .request(
            Request::builder()
                .method("POST")
                .uri("/v1/auth/register")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "username": username,
                        "email": format!("{username}@example.test"),
                        "password": PASSWORD,
                    }))
                    .expect("serialize"),
                ))
                .expect("build request"),
        )
        .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "registration must succeed"
    );
    let session = response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find_map(|line| {
            line.split(';')
                .next()?
                .strip_prefix(&format!("{REFRESH_COOKIE}="))
                .map(str::to_owned)
        })
        .unwrap_or_default();

    let user = tankovault_db::repo::users::find_credentials(&app.db.pool, username)
        .await
        .expect("read the new account")
        .expect("the account exists")
        .user
        .id;
    (user, session)
}

/// Drive the canary with `grant` and report whether the elevation counted.
async fn elevation_counts(app: &TestApp, bearer: &str, grant: &str) -> bool {
    let (status, body) = app
        .call_elevated("GET", SENSITIVE, Some(bearer), Some(grant), None)
        .await;
    if status == StatusCode::FORBIDDEN {
        assert_eq!(
            body["title"], "step_up_required",
            "a refused elevation must name the step-up as the reason, got {body}"
        );
        return false;
    }
    assert_eq!(
        status,
        StatusCode::OK,
        "the canary answered something other than admitted or refused: {body}"
    );
    true
}

/// An enrolled account holding a live grant, with the elevation proven to work.
async fn elevated_account(app: &TestApp, username: &str) -> (UserId, String, String) {
    let user = register(app, username).await;
    let bearer = app.bearer(user);
    app.seed_totp(user).await;
    let grant = app.step_up(user, StepUpMethod::Totp).await;
    assert!(
        elevation_counts(app, &bearer, &grant).await,
        "the control failed: the grant never counted, so nothing below proves anything"
    );
    (user, bearer, grant)
}

// ---------------------------------------------------------------------------
// The window
// ---------------------------------------------------------------------------

/// An elevation lapses, and a lapsed one buys nothing.
///
/// The window is the only bound on a grant that is neither revoked nor spent — it is what stops
/// one confirmation this morning from authorising a deletion this evening. It lives entirely in
/// the `expires_at` predicate of `find_step_up`; drop the predicate and every elevation the
/// system has ever issued becomes permanent, silently, while every other test still passes.
#[tokio::test]
async fn a_lapsed_elevation_buys_nothing() {
    let app = TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await;
    let user = register(&app, "lapsed").await;
    let bearer = app.bearer(user);
    app.seed_totp(user).await;

    let live = app.step_up(user, StepUpMethod::Totp).await;
    assert!(
        elevation_counts(&app, &bearer, &live).await,
        "a grant inside its window must count, or this test proves nothing"
    );

    let lapsed = app
        .step_up_expiring_at(
            user,
            StepUpMethod::Totp,
            time::OffsetDateTime::now_utc() - time::Duration::seconds(1),
        )
        .await;
    assert!(
        !elevation_counts(&app, &bearer, &lapsed).await,
        "a grant past its expiry must not count"
    );
}

// ---------------------------------------------------------------------------
// The three revocations
// ---------------------------------------------------------------------------

/// Signing out ends the elevation, not just the session.
///
/// The scenario is a shared machine. Someone confirms themselves, signs out, and walks away;
/// the next person at the keyboard signs in as themselves. If the grant outlived the sign-out
/// it would be a live elevation nobody at the keyboard earned — and the SPA holds grants in
/// memory precisely because they are meant to be that short-lived.
#[tokio::test]
async fn signing_out_ends_the_elevation_it_leaves_behind() {
    let app = TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await;
    let (user, session) = register_with_session(&app, "departing").await;
    let bearer = app.bearer(user);
    app.seed_totp(user).await;
    let grant = app.step_up(user, StepUpMethod::Totp).await;
    assert!(elevation_counts(&app, &bearer, &grant).await, "control");

    // A live session is what logout reads the family to revoke from; without one the handler
    // clears nothing and this test would pass for the wrong reason.
    assert!(
        !session.is_empty(),
        "registration must issue the session logout is about to end"
    );
    let response = app
        .request(
            Request::builder()
                .method("POST")
                .uri("/v1/auth/logout")
                .header(header::COOKIE, format!("{REFRESH_COOKIE}={session}"))
                .body(Body::empty())
                .expect("build request"),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK, "logout must succeed");

    assert!(
        !elevation_counts(&app, &bearer, &grant).await,
        "a grant that survived the sign-out is an elevation the next person inherits"
    );
}

/// Changing the password ends every elevation the account holds.
///
/// A password change is what someone does when they believe they have been compromised. It
/// already revokes every session; leaving the elevations behind would hand the attacker the one
/// credential that opens the *irreversible* routes — erase the account, change the email, enrol
/// their own factor — for the rest of the window, from a session the owner has just killed.
#[tokio::test]
async fn changing_the_password_ends_every_elevation() {
    let app = TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await;
    let (_, bearer, grant) = elevated_account(&app, "rekeyed").await;

    let (status, body) = app
        .call_elevated(
            "POST",
            "/v1/me/password",
            Some(&bearer),
            Some(&grant),
            Some(json!({ "new_password": "a different correct horse battery" })),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "the password change must succeed: {body}"
    );

    assert!(
        !elevation_counts(&app, &bearer, &grant).await,
        "the elevation must not survive the password it was held alongside"
    );
}

/// Removing the factor that earned an elevation ends the elevation.
///
/// Otherwise removal is a laundering step: present the factor once, drop it, and keep an
/// elevation nothing on the account can any longer produce — which is exactly the position an
/// attacker who has borrowed a factor for one minute wants to be in. The account is left with a
/// live proof of a credential it no longer holds.
#[tokio::test]
async fn removing_the_factor_that_earned_it_ends_the_elevation() {
    let app = TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await;
    let (_, bearer, grant) = elevated_account(&app, "unenrolled").await;

    let (status, body) = app
        .call_elevated(
            "DELETE",
            "/v1/me/mfa/totp",
            Some(&bearer),
            Some(&grant),
            None,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "removing the factor must succeed: {body}"
    );

    assert!(
        !elevation_counts(&app, &bearer, &grant).await,
        "the elevation must not outlive the factor that produced it"
    );
}

// ---------------------------------------------------------------------------
// Values that are not elevations
// ---------------------------------------------------------------------------

/// A grant belonging to a suspended account is not an elevation.
///
/// Suspension is checked in `AuthUser`, ahead of the elevation, so this holds today for free —
/// which is the point of pinning it. An elevation resolved before the account status, or by a
/// handler that reached for the grant directly, would let a banned operator finish whatever they
/// had confirmed themselves for on the way out.
#[tokio::test]
async fn a_suspended_account_cannot_spend_the_elevation_it_already_held() {
    let app = TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await;
    let user = app.seed_user("outgoing", &[], AccountStatus::Active).await;
    let bearer = app.bearer(user);
    app.seed_totp(user).await;
    let grant = app.step_up(user, StepUpMethod::Totp).await;
    assert!(elevation_counts(&app, &bearer, &grant).await, "control");

    tankovault_db::repo::user_admin::set_status(
        &app.db.pool,
        user,
        AccountStatus::Suspended,
        Some("step_up_bypass"),
    )
    .await
    .expect("suspend the account");

    let (status, body) = app
        .call_elevated("GET", SENSITIVE, Some(&bearer), Some(&grant), None)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        body["title"], "account_suspended",
        "suspension must be the reason, ahead of the elevation, got {body}"
    );
}

/// The grant is looked up, never trusted.
///
/// Two failure modes in one assertion. A value that reached an `unwrap` would be a `500` on a
/// header an attacker fully controls; a value that satisfied the extractor without matching a
/// row would make the *header itself* the credential. Both read as "unknown grant" here, and
/// both must read as "no elevation" — the same answer as sending nothing at all.
#[tokio::test]
async fn a_grant_nobody_issued_is_not_an_elevation() {
    let app = TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await;
    let user = register(&app, "forger").await;
    let bearer = app.bearer(user);
    app.seed_totp(user).await;

    // A real grant is a `generate_handle` string, so these cover an empty value, a plausible
    // one, and the shapes a value that ends up in SQL or a path would break on.
    for forged in [
        "",
        "not-a-grant",
        "' OR 1=1 --",
        "../../etc/passwd",
        &"a".repeat(256),
    ] {
        let (status, body) = app
            .call_elevated("GET", SENSITIVE, Some(&bearer), Some(forged), None)
            .await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "a forged grant ({forged:?}) must be refused, got {status} {body}"
        );
        assert_eq!(
            body["title"], "step_up_required",
            "a forged grant ({forged:?}) must read as absent, not as an error"
        );
    }
}
