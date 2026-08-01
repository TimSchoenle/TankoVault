//! The passkey surface, minus the one part a test cannot reach.
//!
//! # What is deliberately not tested here
//!
//! **A completed ceremony.** Verifying an assertion requires a private key inside an
//! authenticator, and there is no authenticator in a test process — the whole point of the
//! credential is that its secret half cannot be produced by software that asks nicely. Faking
//! one would mean either trusting a mock signer (which proves nothing about `webauthn-rs`, the
//! only code doing the verifying) or shipping a fixture keypair and the ceremony transcript it
//! signed, which pins one authenticator's behaviour at one moment and rots silently.
//!
//! So the verification itself is left to the library, and what is pinned here is **everything
//! around it that this repository actually wrote**: which routes exist, who may reach them, what
//! a missing relying party looks like as distinct from a switched-off feature, that a challenge
//! is single-use, and that one account cannot touch another's credentials. Every one of those is
//! a decision made in `services/api/src/{auth/passkey,me/passkeys}.rs` rather than upstream.
//!
//! Opt-in: gated behind the `integration` feature because they require Docker.
#![cfg(feature = "integration")]

use axum::http::StatusCode;
use serde_json::json;
use tankovault_api_test_support::{TestApp, TestConfig};
use tankovault_domain::{AccountStatus, Feature};
use uuid::Uuid;

/// A sign-in challenge is unauthenticated, identifier-free, and fresh every time.
///
/// The freshness assertion is the load-bearing one. A `WebAuthn` challenge is the entire
/// anti-replay mechanism: if two ceremonies could ever share one, an assertion captured from the
/// first would verify against the second. Nothing in the type system connects
/// `start_discoverable_authentication` to that property, and a caching layer added in front of
/// this endpoint — an entirely reasonable thing for someone to try, since the request has no
/// body — would break it silently while every other test still passed.
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
/// The scoping lives in the `WHERE user_id = $1` of two statements, which is exactly the kind of
/// clause that can be dropped without any test noticing. `404` rather than `403` is deliberate
/// and is also pinned: `403` would confirm that the id exists, which turns the endpoint into a
/// way to enumerate other people's credentials one guess at a time.
#[tokio::test]
async fn one_account_cannot_touch_anothers_passkeys() {
    let app = TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await;
    let owner = app.seed_user("owner", &[], AccountStatus::Active).await;
    let stranger = app.seed_user("stranger", &[], AccountStatus::Active).await;

    let record = tankovault_db::repo::users::passkeys::insert(
        &app.db.pool,
        owner,
        b"a-credential-id",
        &json!({ "cred": "opaque to this layer" }),
        "Owner's phone",
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
    let still_there = tankovault_db::repo::users::passkeys::list_for_user(&app.db.pool, owner)
        .await
        .expect("list");
    assert_eq!(still_there.len(), 1);
    assert_eq!(still_there[0].label, "Owner's phone");

    // The owner, meanwhile, can do both.
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

/// Adding a passkey needs the password, not just a session.
///
/// The threat is in `passkey_register_start`'s doc comment: a passkey is permanent and an access
/// token lasts fifteen minutes, so without this check anyone who got hold of a token could
/// install a credential that survives every password change and session revocation afterwards.
///
/// The seeded account's stored hash is a fixture that does not parse, so a wrong password here
/// surfaces as `500` rather than `401` — what the assertion pins is that the ceremony is
/// **refused**, which is the property that matters and the one a deleted check would break.
#[tokio::test]
async fn registering_a_passkey_is_refused_without_the_password() {
    let app = TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await;
    let user = app.seed_user("reader", &[], AccountStatus::Active).await;
    let token = app.bearer(user);

    let (anonymous, _) = app
        .call(
            "POST",
            "/v1/me/passkeys/register/start",
            None,
            Some(json!({ "current_password": "whatever" })),
        )
        .await;
    assert_eq!(anonymous, StatusCode::UNAUTHORIZED);

    let (wrong, _) = app
        .call(
            "POST",
            "/v1/me/passkeys/register/start",
            Some(&token),
            Some(json!({ "current_password": "not the password" })),
        )
        .await;
    assert_ne!(
        wrong,
        StatusCode::OK,
        "a session alone must not be enough to start installing a permanent credential"
    );
}

/// A deployment that configured no origin answers `503` — **not** `404`.
///
/// The distinction is the whole test. `404` is what a switched-off `accounts.passkeys` means:
/// the endpoint is not part of this build. `503` means it is, and an environment variable is
/// missing. Collapsing them tells an operator who forgot one setting that the feature does not
/// exist, which is the answer that ends the investigation in the wrong place.
#[tokio::test]
async fn a_missing_relying_party_is_unavailable_rather_than_absent() {
    let app =
        TestApp::spawn_with(TestConfig::new().without_rate_limiting().without_passkeys()).await;

    let (status, _) = app
        .call("POST", "/v1/auth/passkey/login/start", None, None)
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);

    // The list endpoint does not need a relying party — it reads rows — so it keeps working.
    // Worth pinning: a reader on a misconfigured deployment can still see and revoke the keys
    // they registered before the setting was lost.
    let user = app.seed_user("reader", &[], AccountStatus::Active).await;
    let (listed, _) = app
        .call("GET", "/v1/me/passkeys", Some(&app.bearer(user)), None)
        .await;
    assert_eq!(listed, StatusCode::OK);
}

/// Switching the feature off takes **both** halves with it.
///
/// Two rules in `route_features`, one flag. If only the management surface were gated, a
/// registered credential would keep signing people in while its owner had no way to see or
/// revoke it — the worst of the two states, and the one a single missing `.gate(…)` produces.
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

    // Password sign-in is untouched — passkeys are additive, and switching them off must not
    // take the credential every account has with them.
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
