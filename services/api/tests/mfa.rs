//! Two-factor authentication end to end: the sign-in second leg, enrolment, recovery codes, and
//! the step-up prompt in front of every sensitive action.
//!
//! What is *not* here is `WebAuthn` signature verification — no authenticator exists in a test
//! process, so that stays `webauthn-rs`'s job, exactly as `passkeys.rs` leaves it. Everything
//! around it is here: which routes demand what, which refusals are distinguishable, and the
//! handful of rules whose absence would be silent.
//!
//! Opt-in: gated behind the `integration` feature because they require Docker.
#![cfg(feature = "integration")]

use axum::http::StatusCode;
use serde_json::{Value, json};
use tankovault_api_test_support::{TestApp, TestConfig, totp_code};
use tankovault_db::repo::users::mfa::StepUpMethod;
use tankovault_domain::{AccountStatus, UserId};

/// The password every seeded account in this file is created with.
const PASSWORD: &str = "correct horse battery staple";

/// Register an account through the real sign-up route, so its password hash is one the login
/// path can actually verify.
///
/// `TestDb::seed_user` writes a placeholder hash — fine for suites that only ever mint bearer
/// tokens directly, useless here, where the whole subject is what happens *during* a sign-in.
async fn register(app: &TestApp, username: &str) -> UserId {
    let (status, _) = app
        .call(
            "POST",
            "/v1/auth/register",
            None,
            Some(json!({
                "username": username,
                "email": format!("{username}@example.test"),
                "password": PASSWORD,
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "registration must succeed");

    tankovault_db::repo::users::find_credentials(&app.db.pool, username)
        .await
        .expect("read the new account")
        .expect("the account exists")
        .user
        .id
}

async fn login(app: &TestApp, username: &str) -> (StatusCode, Value) {
    app.call(
        "POST",
        "/v1/auth/login",
        None,
        Some(json!({ "login": username, "password": PASSWORD })),
    )
    .await
}

// ---------------------------------------------------------------------------
// The sign-in second leg
// ---------------------------------------------------------------------------

/// A password alone stops being a session the moment a second factor exists.
///
/// This is the whole feature in one test. The failure it guards against is not a wrong status
/// code — it is `login` continuing to hand out a token *and* a challenge, which would look
/// correct in a browser (the user is signed in, and is also asked for a code) while leaving the
/// password a complete credential for anyone who skipped the prompt.
#[tokio::test]
async fn a_password_alone_yields_a_challenge_and_no_session_once_a_factor_is_enrolled() {
    let app = TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await;
    let user = register(&app, "enrolled").await;

    // Before enrolment: a session, as it always was.
    let (status, body) = login(&app, "enrolled").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "authenticated");
    assert!(body["session"]["access_token"].is_string(), "{body}");
    assert!(body["mfa"].is_null());

    let secret = app.seed_totp(user).await;

    let (status, body) = login(&app, "enrolled").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "mfa_required");
    assert!(
        body["session"].is_null(),
        "a half-finished sign-in must carry no session at all, got {body}"
    );
    assert!(
        body["mfa"]["challenge_token"].is_string(),
        "the client needs a handle to finish with, got {body}"
    );
    assert_eq!(
        body["mfa"]["methods"],
        json!(["totp"]),
        "the challenge must offer exactly the factors this account can present; `seed_totp` \
         writes the enrolment directly and issues no recovery codes, so there are none to offer"
    );

    let challenge = body["mfa"]["challenge_token"].as_str().expect("a handle");
    let (status, body) = app
        .call(
            "POST",
            "/v1/auth/mfa/verify",
            None,
            Some(json!({ "challenge_token": challenge, "totp_code": totp_code(&secret) })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["access_token"].is_string(),
        "the second leg issues the session the first withheld, got {body}"
    );
}

/// A six-digit code is only a second factor if the guesses are counted.
///
/// The bug this pins is subtle and total: count failures in the handler's error path instead of
/// in the statement that resolves the challenge, and any early return — a malformed body, a
/// dropped connection, a client that simply does not read the response — skips the increment.
/// The challenge then lives its full five minutes at a million guesses, which is not a second
/// factor, and every functional test still passes because a *correct* code still works.
#[tokio::test]
async fn a_challenge_is_destroyed_once_its_attempts_are_spent() {
    let app = TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await;
    let user = register(&app, "bruteforced").await;
    let secret = app.seed_totp(user).await;

    let (_, body) = login(&app, "bruteforced").await;
    let challenge = body["mfa"]["challenge_token"]
        .as_str()
        .expect("a handle")
        .to_owned();

    for attempt in 1..=6 {
        let (status, _) = app
            .call(
                "POST",
                "/v1/auth/mfa/verify",
                None,
                Some(json!({ "challenge_token": challenge, "totp_code": "000000" })),
            )
            .await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "attempt {attempt} must be refused"
        );
    }

    // Spent: even the right code no longer finishes this sign-in.
    let (status, _) = app
        .call(
            "POST",
            "/v1/auth/mfa/verify",
            None,
            Some(json!({ "challenge_token": challenge, "totp_code": totp_code(&secret) })),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "an exhausted challenge must be gone, not merely failing"
    );
}

/// One challenge yields at most one session.
#[tokio::test]
async fn a_challenge_is_consumed_by_the_sign_in_it_completes() {
    let app = TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await;
    let user = register(&app, "singleuse").await;
    let secret = app.seed_totp(user).await;

    let (_, body) = login(&app, "singleuse").await;
    let challenge = body["mfa"]["challenge_token"]
        .as_str()
        .expect("a handle")
        .to_owned();

    let code = totp_code(&secret);
    let (first, _) = app
        .call(
            "POST",
            "/v1/auth/mfa/verify",
            None,
            Some(json!({ "challenge_token": challenge, "totp_code": code })),
        )
        .await;
    assert_eq!(first, StatusCode::OK);

    let (replayed, _) = app
        .call(
            "POST",
            "/v1/auth/mfa/verify",
            None,
            Some(json!({ "challenge_token": challenge, "totp_code": code })),
        )
        .await;
    assert_eq!(
        replayed,
        StatusCode::UNAUTHORIZED,
        "the challenge survived the sign-in it completed"
    );
}

/// Passkey sign-in stays a **single** leg, however many factors the account holds.
///
/// The asymmetry is deliberate and is exactly the kind a later reviewer "fixes" for
/// consistency. A passkey is already phishing-resistant and user-verified; asking for a code
/// after one teaches users that the strongest credential is the weakest. The endpoint is driven
/// with an unsigned assertion here — it cannot verify, and that is fine: what is pinned is that
/// it answers `401` rather than `200 mfa_required`, i.e. that it never grew a second leg.
#[tokio::test]
async fn a_passkey_sign_in_is_never_asked_for_a_second_factor() {
    let app = TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await;
    let user = register(&app, "passkeyed").await;
    app.seed_totp(user).await;

    let (_, started) = app
        .call("POST", "/v1/auth/passkey/login/start", None, None)
        .await;
    let (status, body) = app
        .call(
            "POST",
            "/v1/auth/passkey/login/finish",
            None,
            Some(json!({
                "ceremony_id": started["ceremony_id"],
                "credential": {
                    "id": "AAAA",
                    "rawId": "AAAA",
                    "type": "public-key",
                    "response": {
                        "authenticatorData": "AAAA",
                        "clientDataJSON": "AAAA",
                        "signature": "AAAA",
                        "userHandle": "AAAA",
                    },
                },
            })),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // A problem document, not a `LoginResponse`. `status` is checked for the *string*
    // `mfa_required` rather than for absence, because a problem body carries a numeric `status`
    // of its own and asserting absence would pass for the wrong reason.
    assert!(
        body.get("mfa").is_none() && body["status"] != json!("mfa_required"),
        "the passkey path must not have grown a second leg, got {body}"
    );
}

// ---------------------------------------------------------------------------
// Enrolment and recovery codes
// ---------------------------------------------------------------------------

/// Enrolment is two steps, and only the second one produces a factor.
///
/// The bug this pins: treating the row written by `POST /v1/me/mfa/totp` as an enrolment. The
/// user has been *shown* a secret at that point and may have closed the tab before storing it —
/// counting it would demand a code at the next sign-in that nobody on earth can produce.
#[tokio::test]
async fn an_unconfirmed_enrolment_is_not_a_factor() {
    let app = TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await;
    let user = register(&app, "halfway").await;
    let token = app.bearer(user);

    let (status, body) = app
        .call("POST", "/v1/me/mfa/totp", Some(&token), Some(json!({})))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["secret"].is_string());
    assert!(
        body["provisioning_uri"]
            .as_str()
            .is_some_and(|u| u.starts_with("otpauth://totp/")),
        "the client renders this as a QR code, got {body}"
    );

    let (_, status_body) = app.call("GET", "/v1/me/mfa", Some(&token), None).await;
    assert_eq!(
        status_body["enrolled"], false,
        "an unconfirmed enrolment must not read as a factor, got {status_body}"
    );
    assert!(status_body["totp_confirmed_at"].is_null());

    // And the sign-in path agrees, which is the half that would lock the account out.
    let (_, login_body) = login(&app, "halfway").await;
    assert_eq!(login_body["status"], "authenticated");
}

/// Confirming the first factor issues recovery codes, once.
#[tokio::test]
async fn confirming_the_first_factor_issues_recovery_codes() {
    let app = TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await;
    let user = register(&app, "recoverable").await;
    let token = app.bearer(user);

    let (_, begun) = app
        .call("POST", "/v1/me/mfa/totp", Some(&token), Some(json!({})))
        .await;
    let secret = secret_from(&begun);

    let (status, body) = app
        .call(
            "POST",
            "/v1/me/mfa/totp/confirm",
            Some(&token),
            Some(json!({ "code": totp_code(&secret) })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let codes = body["codes"].as_array().expect("a set of codes");
    assert_eq!(codes.len(), 10, "a set is ten codes, got {body}");

    let (_, status_body) = app.call("GET", "/v1/me/mfa", Some(&token), None).await;
    assert_eq!(status_body["enrolled"], true);
    assert_eq!(status_body["recovery_codes_remaining"], 10);
}

/// A recovery code signs its owner in exactly once.
///
/// The bug this pins: verifying a code without consuming it. A printed sheet would then be a
/// permanent bypass of the second factor rather than an escape hatch — and the account page
/// would keep reporting ten codes remaining while one of them let anybody in forever.
#[tokio::test]
async fn a_recovery_code_signs_in_once_and_never_again() {
    let app = TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await;
    let user = register(&app, "locked-out").await;
    let token = app.bearer(user);

    let (_, begun) = app
        .call("POST", "/v1/me/mfa/totp", Some(&token), Some(json!({})))
        .await;
    let secret = secret_from(&begun);
    let (_, confirmed) = app
        .call(
            "POST",
            "/v1/me/mfa/totp/confirm",
            Some(&token),
            Some(json!({ "code": totp_code(&secret) })),
        )
        .await;
    let code = confirmed["codes"][0]
        .as_str()
        .expect("a recovery code")
        .to_owned();

    let (_, body) = login(&app, "locked-out").await;
    let challenge = body["mfa"]["challenge_token"].as_str().expect("a handle");
    let (status, _) = app
        .call(
            "POST",
            "/v1/auth/mfa/verify",
            None,
            Some(json!({ "challenge_token": challenge, "recovery_code": code })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let (_, body) = login(&app, "locked-out").await;
    let challenge = body["mfa"]["challenge_token"].as_str().expect("a handle");
    let (status, _) = app
        .call(
            "POST",
            "/v1/auth/mfa/verify",
            None,
            Some(json!({ "challenge_token": challenge, "recovery_code": code })),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "a spent recovery code must never work again"
    );

    let (_, status_body) = app.call("GET", "/v1/me/mfa", Some(&token), None).await;
    assert_eq!(status_body["recovery_codes_remaining"], 9);
}

/// Replacing a *confirmed* enrolment is refused; removing it is not.
///
/// The attack this closes: a stolen session silently re-enrolling TOTP against the attacker's
/// own authenticator, which would replace the owner's factor and lock them out using the very
/// mechanism meant to protect them. Removal is allowed, but only behind a step-up — which the
/// attacker cannot satisfy, because satisfying it needs the factor they are trying to replace.
#[tokio::test]
async fn a_confirmed_enrolment_cannot_be_silently_replaced() {
    let app = TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await;
    let user = register(&app, "settled").await;
    let token = app.bearer(user);
    app.seed_totp(user).await;

    let elevated = app.step_up(user, StepUpMethod::Totp).await;
    let (status, body) = app
        .call_elevated(
            "POST",
            "/v1/me/mfa/totp",
            Some(&token),
            Some(&elevated),
            Some(json!({})),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a confirmed enrolment must not be overwritten, got {body}"
    );

    // Without the elevation it does not even get that far.
    let (unelevated, body) = app
        .call("POST", "/v1/me/mfa/totp", Some(&token), Some(json!({})))
        .await;
    assert_eq!(unelevated, StatusCode::FORBIDDEN);
    assert_eq!(body["title"], "step_up_required");

    // Removal, elevated, works — and takes the elevation with it.
    let elevated = app.step_up(user, StepUpMethod::Totp).await;
    let (removed, _) = app
        .call_elevated(
            "DELETE",
            "/v1/me/mfa/totp",
            Some(&token),
            Some(&elevated),
            None,
        )
        .await;
    assert_eq!(removed, StatusCode::NO_CONTENT);
    let (reused, _) = app
        .call_elevated(
            "POST",
            "/v1/me/mfa/recovery-codes",
            Some(&token),
            Some(&elevated),
            None,
        )
        .await;
    assert_eq!(
        reused,
        StatusCode::FORBIDDEN,
        "a grant must not outlive the factor that earned it"
    );
}

// ---------------------------------------------------------------------------
// Step-up
// ---------------------------------------------------------------------------

/// A sensitive route refuses without an elevation and answers with it.
///
/// `403` with a distinct problem type, not `401`: the caller *is* authenticated, and a `401`
/// would drive the SPA's sign-out path — turning "confirm it is you" into "you have been logged
/// out". The type is asserted for that reason, not the status.
#[tokio::test]
async fn a_sensitive_route_demands_an_elevation_and_names_the_reason() {
    let app = TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await;
    let user = register(&app, "sensitive").await;
    let token = app.bearer(user);
    let secret = app.seed_totp(user).await;

    let (status, body) = app.call("GET", "/v1/me/export", Some(&token), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["title"], "step_up_required");
    assert_eq!(body["type"], "about:blank#step_up_required");

    let (status, grant) = app
        .call(
            "POST",
            "/v1/me/step-up",
            Some(&token),
            Some(json!({ "totp_code": totp_code(&secret) })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{grant}");
    let elevation = grant["token"].as_str().expect("a grant token");

    let (status, _) = app
        .call_elevated("GET", "/v1/me/export", Some(&token), Some(elevation), None)
        .await;
    assert_eq!(status, StatusCode::OK);
}

/// The password fallback exists for accounts with no factor — and stops the moment one does.
///
/// Both halves matter. Without the fallback, an account that has never enrolled cannot reach
/// the sensitive routes at all, *including the enrolment that would fix that*. Without the
/// cut-off, enrolling would leave the weaker proof usable beside the stronger one, and every
/// elevation would be worth exactly what the password is worth against someone who has it.
#[tokio::test]
async fn a_password_elevation_stops_counting_once_a_factor_exists() {
    let app = TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await;
    let user = register(&app, "fallback").await;
    let token = app.bearer(user);

    let (status, grant) = app
        .call(
            "POST",
            "/v1/me/step-up",
            Some(&token),
            Some(json!({ "password": PASSWORD })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{grant}");
    let elevation = grant["token"].as_str().expect("a grant token").to_owned();

    let (status, _) = app
        .call_elevated("GET", "/v1/me/export", Some(&token), Some(&elevation), None)
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the fallback must work while unenrolled"
    );

    app.seed_totp(user).await;

    let (status, body) = app
        .call_elevated("GET", "/v1/me/export", Some(&token), Some(&elevation), None)
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a password-earned grant must stop counting once a factor exists, got {body}"
    );

    // And it cannot be re-earned that way either.
    let (status, _) = app
        .call(
            "POST",
            "/v1/me/step-up",
            Some(&token),
            Some(json!({ "password": PASSWORD })),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// An elevation belongs to the account that earned it.
#[tokio::test]
async fn one_account_cannot_present_anothers_elevation() {
    let app = TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await;
    let owner = register(&app, "owner").await;
    let stranger = register(&app, "stranger").await;
    let elevation = app.step_up(owner, StepUpMethod::Password).await;

    let (status, _) = app
        .call_elevated(
            "GET",
            "/v1/me/export",
            Some(&app.bearer(stranger)),
            Some(&elevation),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

// ---------------------------------------------------------------------------
// Privileged accounts
// ---------------------------------------------------------------------------

/// An administrator without a second factor cannot administer anything.
///
/// Enforced in `AuthUser::require_all`, which every privileged handler funnels through, so this
/// holds for a route nobody thought about — including one added outside `/v1/admin`. The
/// refusal is distinguishable from "insufficient privileges" on purpose: the console routes the
/// operator to enrolment rather than showing them a permissions error they cannot act on.
#[tokio::test]
async fn a_privileged_account_must_hold_a_second_factor() {
    let app = TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await;
    let admin = app
        .seed_user(
            "unenrolled-admin",
            &[tankovault_domain::Permission::SystemStats],
            AccountStatus::Active,
        )
        .await;
    let token = app.bearer(admin);

    let (status, body) = app.call("GET", "/v1/admin/stats", Some(&token), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        body["title"], "mfa_enrolment_required",
        "an unenrolled administrator must be told to enrol, not that their grants are wrong"
    );

    app.seed_totp(admin).await;
    let (status, _) = app.call("GET", "/v1/admin/stats", Some(&token), None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a read needs the factor enrolled, not presented"
    );
}

/// A *mutating* capability additionally needs the factor presented; a read does not.
///
/// The line is drawn by `Permission::is_mutating`, exhaustively. Prompting to load a dashboard
/// would keep a standing elevation open all day, which is worse than not prompting at all — so
/// reads are deliberately exempt, and this test is what stops that exemption widening.
#[tokio::test]
async fn an_administrative_write_needs_an_elevation_and_a_read_does_not() {
    let app = TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await;
    let admin = app
        .seed_user(
            "flagger",
            &[
                tankovault_domain::Permission::FlagsRead,
                tankovault_domain::Permission::FlagsWrite,
            ],
            AccountStatus::Active,
        )
        .await;
    let token = app.bearer(admin);
    app.seed_totp(admin).await;

    let (read, _) = app
        .call("GET", "/v1/admin/feature-flags", Some(&token), None)
        .await;
    assert_eq!(read, StatusCode::OK, "a read must not prompt");

    let write = json!({ "enabled": false });
    let (unelevated, body) = app
        .call(
            "PUT",
            "/v1/admin/feature-flags/accounts.passkeys",
            Some(&token),
            Some(write.clone()),
        )
        .await;
    assert_eq!(unelevated, StatusCode::FORBIDDEN);
    assert_eq!(body["title"], "step_up_required");

    let elevation = app.step_up(admin, StepUpMethod::Totp).await;
    let (elevated, body) = app
        .call_elevated(
            "PUT",
            "/v1/admin/feature-flags/accounts.passkeys",
            Some(&token),
            Some(&elevation),
            Some(write),
        )
        .await;
    assert!(
        elevated.is_success(),
        "an elevated write must go through, got {elevated} {body}"
    );
}

/// Read a base32 secret out of an enrolment response.
fn secret_from(body: &Value) -> secrecy::SecretSlice<u8> {
    let encoded = body["secret"].as_str().expect("an enrolment secret");
    secrecy::SecretSlice::from(
        data_encoding::BASE32_NOPAD
            .decode(encoded.as_bytes())
            .expect("the secret is unpadded base32"),
    )
}
