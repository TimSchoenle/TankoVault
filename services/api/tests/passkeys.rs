//! The passkey surface, minus ceremony verification (no authenticator exists in a test process,
//! so that part is left to `webauthn-rs`). Pins routes, access scoping, single-use challenges,
//! and the missing-relying-party vs. disabled-feature distinction instead.
//!
//! Opt-in: gated behind the `integration` feature because they require Docker.
#![cfg(feature = "integration")]

use axum::http::StatusCode;
use serde_json::json;
use tankovault_api_test_support::{TestApp, TestConfig};
use tankovault_db::repo::users::webauthn::CredentialPurpose;
use tankovault_domain::{AccountStatus, Feature};
use uuid::Uuid;

/// A sign-in challenge is unauthenticated, identifier-free, and fresh every time.
///
/// Freshness is load-bearing: a `WebAuthn` challenge is the whole anti-replay mechanism, and a
/// caching layer placed in front of this bodyless endpoint would break it silently while every
/// other test still passed.
#[tokio::test]
async fn a_sign_in_challenge_needs_no_credential_and_is_never_reused() {
    let app = TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await;

    let (status, first) = app
        .call("POST", "/v1/auth/passkey/login/start", None, None)
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the sign-in challenge must be reachable signed out; that is the point of it"
    );

    let (_, second) = app
        .call("POST", "/v1/auth/passkey/login/start", None, None)
        .await;

    assert_ne!(
        first["ceremony_id"], second["ceremony_id"],
        "two ceremonies shared a handle"
    );
    let challenge = |body: &serde_json::Value| {
        body["options"]["publicKey"]["challenge"]
            .as_str()
            .expect("the envelope carries a challenge")
            .to_owned()
    };
    assert_ne!(
        challenge(&first),
        challenge(&second),
        "a repeated challenge makes a captured assertion replayable"
    );

    // No account was named, so the envelope must not name one either.
    assert!(
        first["options"]["publicKey"]["allowCredentials"]
            .as_array()
            .is_none_or(Vec::is_empty),
        "a discoverable sign-in must not disclose which credentials exist"
    );
}

/// A challenge is consumed by its first use, and an unknown handle is refused.
///
/// Both answer `401`, and both must: the response cannot distinguish "already used" from "never
/// existed" from "expired", or a client could enumerate live ceremonies.
#[tokio::test]
async fn a_ceremony_handle_is_single_use_and_unguessable() {
    let app = TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await;

    let (_, started) = app
        .call("POST", "/v1/auth/passkey/login/start", None, None)
        .await;
    let ceremony_id = started["ceremony_id"].as_str().expect("a ceremony id");

    // A structurally valid but unsigned assertion. It cannot verify — that is fine; what is
    // being pinned is that the *challenge* is gone afterwards, whatever the outcome was.
    let assertion = |id: &str| {
        json!({
            "ceremony_id": id,
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
        })
    };

    let (first, _) = app
        .call(
            "POST",
            "/v1/auth/passkey/login/finish",
            None,
            Some(assertion(ceremony_id)),
        )
        .await;
    assert_eq!(first, StatusCode::UNAUTHORIZED);

    let (replayed, _) = app
        .call(
            "POST",
            "/v1/auth/passkey/login/finish",
            None,
            Some(assertion(ceremony_id)),
        )
        .await;
    assert_eq!(
        replayed,
        StatusCode::UNAUTHORIZED,
        "the challenge survived its own use"
    );

    let (unknown, _) = app
        .call(
            "POST",
            "/v1/auth/passkey/login/finish",
            None,
            Some(assertion(&Uuid::new_v4().to_string())),
        )
        .await;
    assert_eq!(
        unknown,
        StatusCode::UNAUTHORIZED,
        "an unknown handle must be indistinguishable from a spent one"
    );
}

/// The management surface is the caller's own, and nobody else's.
///
/// A fresh account has no passkeys — and reads an empty list rather than a `404`, because having
/// none is the ordinary state of most accounts.
#[tokio::test]
async fn the_passkey_list_requires_a_session_and_starts_empty() {
    let app = TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await;
    let user = app.seed_user("reader", &[], AccountStatus::Active).await;
    let token = app.bearer(user);

    let (anonymous, _) = app.call("GET", "/v1/me/passkeys", None, None).await;
    assert_eq!(anonymous, StatusCode::UNAUTHORIZED);

    let (status, body) = app.call("GET", "/v1/me/passkeys", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body.as_array().map(Vec::len),
        Some(0),
        "no passkeys is an empty list, not an error"
    );
}

/// Renaming or revoking another account's passkey is a `404`, not a `403`.
///
/// Scoping lives in a `WHERE user_id = $1` a future edit could silently drop. `404` also
/// matters: a `403` would confirm the id exists, letting the endpoint enumerate credentials.
#[tokio::test]
async fn one_account_cannot_touch_anothers_passkeys() {
    let app = TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await;
    let owner = app.seed_user("owner", &[], AccountStatus::Active).await;
    let stranger = app.seed_user("stranger", &[], AccountStatus::Active).await;

    let record = tankovault_db::repo::users::webauthn::insert(
        &app.db.pool,
        owner,
        b"a-credential-id",
        &json!({ "cred": "opaque to this layer" }),
        "Owner's phone",
        CredentialPurpose::Passkey,
    )
    .await
    .expect("seed a passkey");

    let stranger_token = app.bearer(stranger);
    let path = format!("/v1/me/passkeys/{}", record.id);

    let (renamed, _) = app
        .call(
            "PATCH",
            &path,
            Some(&stranger_token),
            Some(json!({ "label": "mine now" })),
        )
        .await;
    assert_eq!(renamed, StatusCode::NOT_FOUND);

    let (deleted, _) = app.call("DELETE", &path, Some(&stranger_token), None).await;
    assert_eq!(deleted, StatusCode::NOT_FOUND);

    // And the row is untouched, which the status alone does not prove.
    let still_there = tankovault_db::repo::users::webauthn::list_for_user(
        &app.db.pool,
        owner,
        CredentialPurpose::Passkey,
    )
    .await
    .expect("list");
    assert_eq!(still_there.len(), 1);
    assert_eq!(still_there[0].label, "Owner's phone");

    let owner_token = app.bearer(owner);
    let (renamed, _) = app
        .call(
            "PATCH",
            &path,
            Some(&owner_token),
            Some(json!({ "label": "Phone" })),
        )
        .await;
    assert_eq!(renamed, StatusCode::NO_CONTENT);
    let (deleted, _) = app.call("DELETE", &path, Some(&owner_token), None).await;
    assert_eq!(deleted, StatusCode::NO_CONTENT);
}

/// Adding a passkey needs a second factor enrolled **and** presented, not just a session.
///
/// A passkey is permanent while an access token lasts fifteen minutes, and it signs in on its
/// own with no second leg — so minting one is the act of creating a single-factor bypass of
/// everything below it. Three states, three answers:
///
/// * no session at all — `401`;
/// * a session, no factor enrolled — `403 mfa_enrolment_required`, the gate this change exists
///   for. It is checked separately from the elevation because an unenrolled account *can* still
///   elevate, using the password fallback, so the elevation alone does not imply enrolment;
/// * a factor enrolled but not presented — `403 step_up_required`.
///
/// The suite deliberately asserts the problem *type*, not just the status: both refusals are
/// `403`, and a client that cannot tell them apart sends a user to re-authenticate when what
/// they need is to enrol.
#[tokio::test]
async fn registering_a_passkey_needs_a_second_factor_enrolled_and_presented() {
    let app = TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await;
    let user = app.seed_user("reader", &[], AccountStatus::Active).await;
    let token = app.bearer(user);

    let (anonymous, _) = app
        .call(
            "POST",
            "/v1/me/passkeys/register/start",
            None,
            Some(json!({})),
        )
        .await;
    assert_eq!(anonymous, StatusCode::UNAUTHORIZED);

    // Elevated by password, which is all an account with no factor can offer — and still
    // refused, because the gate is on enrolment rather than on the elevation.
    let password_grant = app
        .step_up(
            user,
            tankovault_db::repo::users::mfa::StepUpMethod::Password,
        )
        .await;
    let (unenrolled, body) = app
        .call_elevated(
            "POST",
            "/v1/me/passkeys/register/start",
            Some(&token),
            Some(&password_grant),
            Some(json!({})),
        )
        .await;
    assert_eq!(unenrolled, StatusCode::FORBIDDEN);
    assert_eq!(
        body["type"], "about:blank#mfa_enrolment_required",
        "the client must be able to tell 'enrol first' from 'confirm it is you'"
    );

    // Enrolled, but nothing presented.
    app.seed_totp(user).await;
    let (unelevated, body) = app
        .call(
            "POST",
            "/v1/me/passkeys/register/start",
            Some(&token),
            Some(json!({})),
        )
        .await;
    assert_eq!(unelevated, StatusCode::FORBIDDEN);
    assert_eq!(body["type"], "about:blank#step_up_required");

    // Enrolled and presented: the ceremony starts.
    let grant = app
        .step_up(user, tankovault_db::repo::users::mfa::StepUpMethod::Totp)
        .await;
    let (allowed, body) = app
        .call_elevated(
            "POST",
            "/v1/me/passkeys/register/start",
            Some(&token),
            Some(&grant),
            Some(json!({})),
        )
        .await;
    assert_eq!(allowed, StatusCode::OK, "{body}");
    assert!(body["ceremony_id"].is_string());
}

/// A deployment that configured no origin answers `503`, not `404`.
///
/// `404` means the feature is switched off; `503` means it's on but misconfigured. Collapsing
/// them sends an operator chasing a missing setting to the wrong place.
#[tokio::test]
async fn a_missing_relying_party_is_unavailable_rather_than_absent() {
    let app =
        TestApp::spawn_with(TestConfig::new().without_rate_limiting().without_passkeys()).await;

    let (status, _) = app
        .call("POST", "/v1/auth/passkey/login/start", None, None)
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);

    // No relying party needed to read rows, so a reader can still see and revoke existing keys.
    let user = app.seed_user("reader", &[], AccountStatus::Active).await;
    let (listed, _) = app
        .call("GET", "/v1/me/passkeys", Some(&app.bearer(user)), None)
        .await;
    assert_eq!(listed, StatusCode::OK);
}

/// Switching the feature off takes **both** halves with it.
///
/// Two rules in `route_features`, one flag — gating only the management surface would let a
/// registered credential keep signing people in with no way to revoke it.
#[tokio::test]
async fn switching_passkeys_off_removes_sign_in_and_management_together() {
    let app = TestApp::spawn_with(
        TestConfig::new()
            .without_rate_limiting()
            .with_features_disabled(&[Feature::AccountsPasskeys]),
    )
    .await;
    let user = app.seed_user("reader", &[], AccountStatus::Active).await;
    let token = app.bearer(user);

    for (method, path, bearer) in [
        ("POST", "/v1/auth/passkey/login/start", None),
        ("GET", "/v1/me/passkeys", Some(token.as_str())),
        (
            "POST",
            "/v1/me/passkeys/register/start",
            Some(token.as_str()),
        ),
    ] {
        let (status, body) = app.call(method, path, bearer, Some(json!({}))).await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "{method} {path} stayed reachable with the feature off"
        );
        assert_eq!(body["feature"], "accounts.passkeys");
    }

    // Passkeys are additive; switching them off must not take password sign-in with them.
    let (status, _) = app
        .call(
            "POST",
            "/v1/auth/login",
            None,
            Some(json!({ "login": "reader", "password": "wrong" })),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "password sign-in must still be reachable, and answer on its own terms"
    );
}
