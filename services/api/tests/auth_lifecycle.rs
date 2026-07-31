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

/// The cookie name a deployment that marks cookies `Secure` issues — which the harness now does
/// by default, matching production (SEC-7).
const REFRESH_COOKIE: &str = "__Host-refresh_token";

/// The unprefixed name the local-HTTP development opt-out keeps, at the narrow path.
const DEV_REFRESH_COOKIE: &str = "refresh_token";

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

/// The raw `Set-Cookie` line for the refresh cookie, whatever its value.
fn refresh_set_cookie(resp: &Response<Body>) -> Option<String> {
    set_cookie_named(resp, REFRESH_COOKIE)
}

/// The raw `Set-Cookie` line for `name`, whatever its value.
fn set_cookie_named(resp: &Response<Body>, name: &str) -> Option<String> {
    let prefix = format!("{name}=");
    resp.headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|t| t.starts_with(&prefix))
        .map(str::to_owned)
}

/// The refresh token a response issued, or `None` when it issued none (or cleared it).
fn refresh_token(resp: &Response<Body>) -> Option<String> {
    let line = refresh_set_cookie(resp)?;
    let value = line
        .strip_prefix(&format!("{REFRESH_COOKIE}="))?
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
async fn the_refresh_cookie_carries_every_attribute_the_host_prefix_requires() {
    // These attributes are the whole containment story for a 30-day credential. Previously this
    // test asserted `Path=/v1/auth` and treated a widening to `/` as the regression; SEC-7's
    // review reversed that judgement — nothing outside `auth/` reads a cookie, `SameSite=Strict`
    // already blocks every cross-site send, and the narrow path did nothing against the one
    // attack it looked like it was for (a sibling subdomain *writing* this cookie, which the
    // `__Host-` prefix is what actually prevents).
    //
    // So the assertion is now the prefix's own contract, and it is the load-bearing one: the
    // browser refuses a `__Host-` cookie that lacks `Secure`, carries a `Domain`, or is set at
    // any path but `/`, and it refuses it *silently*. Drop any one of the three and every
    // server-side test still passes while no real browser keeps the cookie — sign-in appears to
    // work and every reload lands signed out.
    let app = app_without_mail().await;
    let resp = register(&app, "cookiewatcher").await;
    let line = refresh_set_cookie(&resp).expect("registration sets the refresh cookie");

    assert!(line.contains("HttpOnly"), "not HttpOnly: {line}");
    assert!(
        line.contains("SameSite=Strict"),
        "not SameSite=Strict: {line}"
    );
    assert!(line.contains("Secure"), "__Host- requires Secure: {line}");
    assert!(
        line.contains("Path=/;") || line.ends_with("Path=/"),
        "__Host- requires exactly Path=/, got {line}"
    );
    assert!(
        !line.contains("Domain="),
        "__Host- forbids Domain, and a Domain-scoped cookie is writable by every sibling \
         subdomain: {line}"
    );
}

#[tokio::test]
async fn the_local_http_opt_out_keeps_the_unprefixed_cookie_at_the_narrow_path() {
    // `cookie_secure = false` exists for plain-HTTP development, where a `Secure` cookie is never
    // sent at all. A `__Host-` name without `Secure` is *refused* rather than downgraded, so that
    // configuration has to keep the old spelling — and the read side must accept only the name
    // this deployment issues, never both, or a sibling subdomain could plant the unprefixed one
    // and have it honoured in production.
    let app = TestApp::spawn_with(
        TestConfig::new()
            .without_rate_limiting()
            .with_insecure_cookies(),
    )
    .await;
    let resp = register(&app, "localdev").await;

    assert!(
        set_cookie_named(&resp, REFRESH_COOKIE).is_none(),
        "the insecure configuration must not issue a `__Host-` cookie the browser would drop"
    );
    let line = set_cookie_named(&resp, DEV_REFRESH_COOKIE).expect("the development cookie");
    assert!(
        line.contains("Path=/v1/auth"),
        "the unprefixed cookie keeps the narrow path, got {line}"
    );
    assert!(!line.contains("Secure"), "opted out of Secure, got {line}");
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
        cleared.starts_with(&format!("{REFRESH_COOKIE}=;")),
        "logout must clear the cookie value, got {cleared}"
    );
    // The removal has to match the path the cookie was set at, or the browser keeps its copy and
    // "log out" only means "forget server-side".
    assert!(
        cleared.contains("Path=/;") || cleared.ends_with("Path=/"),
        "the clearing cookie must name the same path, got {cleared}"
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

/// A reset request answers before doing any of the work only a known address triggers (SEC-10).
///
/// The bug: `forgot_password` returned a uniform `202` but the known-address branch minted a
/// token, hashed it and performed an `INSERT` — a write, so a WAL flush — before answering, while
/// the unknown branch returned straight after the lookup. The response time was the oracle the
/// uniform status was supposed to hide, and reading it needs no credentials and no statistics.
///
/// Timed against a **deliberately slow mailer** rather than against the `INSERT`, which is the
/// only way to make this reliable: one database write is a millisecond or two and would flap, but
/// a two-second relay against a sub-second assertion cannot. If someone reverts the handler to
/// `deliver_reset_link(...).await` instead of spawning it, the known-address response waits for
/// that relay and this fails; the unknown-address one never would, which is exactly the asymmetry
/// under test.
#[tokio::test]
async fn a_reset_request_answers_without_waiting_for_the_known_branch_work() {
    let mailer =
        Arc::new(RecordingMailer::enabled().with_send_delay(std::time::Duration::from_secs(2)));
    let app = TestApp::spawn_with(
        TestConfig::new()
            .with_mailer(mailer.clone())
            .without_rate_limiting(),
    )
    .await;
    let registered = register(&app, "timedout").await;
    assert_eq!(registered.status(), StatusCode::OK);
    // Registration also sends a confirmation; drain it so the reset mail is the next message.
    mailer.next_message().await;

    let known = timed_forgot(&app, "timedout@example.test").await;
    // Drained here, between the two requests, rather than at the end. Two detached tasks in flight
    // at once made this suite's shared test Postgres open a third pooled connection, which blocks
    // when that container is near its own `max_connections` — see the note in `TestDb` (ARCH-6b) on
    // the container being reused across every test binary. Sequencing the requests keeps the test
    // measuring the handler rather than the harness.
    let message = mailer.next_message().await;
    let unknown = timed_forgot(&app, "nobody-at-all@example.test").await;

    assert_eq!(known.0, StatusCode::ACCEPTED);
    assert_eq!(unknown.0, StatusCode::ACCEPTED);
    assert!(
        known.1 < std::time::Duration::from_secs(1),
        "the known-address branch must not do its work on the request path; took {:?}",
        known.1
    );
    assert!(
        unknown.1 < std::time::Duration::from_secs(1),
        "the unknown-address branch took {:?}",
        unknown.1
    );

    // And the work still happens: the detached task must actually deliver, or "fast" would only
    // mean "broken".
    assert!(
        RecordingMailer::first_link(&message.text).is_some_and(|l| l.contains("/reset-password")),
        "the spawned task must still send the reset link"
    );
}

/// The confirmation resend has the same shape, and had the same channel — worse, in fact.
///
/// The audit named `forgot_password`; `resend_verification` performed the token `INSERT` only for
/// an address that exists **and is still unconfirmed**, while both "no such address" and "already
/// confirmed" returned straight after the lookup. So its timing separated three states rather than
/// two, and "this address exists and has not been confirmed yet" is the more useful of the pair to
/// an attacker. Both handlers are spawned in full now.
#[tokio::test]
async fn a_confirmation_resend_answers_without_waiting_for_the_known_branch_work() {
    let mailer =
        Arc::new(RecordingMailer::enabled().with_send_delay(std::time::Duration::from_secs(2)));
    let app = TestApp::spawn_with(
        TestConfig::new()
            .with_mailer(mailer.clone())
            .without_rate_limiting(),
    )
    .await;
    register(&app, "unconfirmed").await;
    mailer.next_message().await;

    let started = std::time::Instant::now();
    let resp = app
        .request(post(
            "/v1/auth/verify-email/resend",
            None,
            Some(json!({ "email": "unconfirmed@example.test" })),
        ))
        .await;
    let elapsed = started.elapsed();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    assert!(
        elapsed < std::time::Duration::from_secs(1),
        "the resend must not do its work on the request path; took {elapsed:?}"
    );

    let message = mailer.next_message().await;
    assert!(
        RecordingMailer::first_link(&message.text).is_some_and(|l| l.contains("/verify-email")),
        "the spawned task must still send a fresh confirmation link"
    );
}

/// Issue one reset request and report its status and how long the caller waited.
async fn timed_forgot(app: &TestApp, email: &str) -> (StatusCode, std::time::Duration) {
    let started = std::time::Instant::now();
    let resp = app
        .request(post(
            "/v1/auth/password/forgot",
            None,
            Some(json!({ "email": email })),
        ))
        .await;
    (resp.status(), started.elapsed())
}
