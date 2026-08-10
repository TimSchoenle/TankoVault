//! What `accounts.required` does to a real request: that the layer is mounted, that it refuses
//! an anonymous caller everywhere the wall applies, and — the leg that keeps it a feature rather
//! than a lockout — that the surface a visitor needs in order to *get* an account stays open.
//!
//! `services/api/src/account_gate.rs`'s unit tests cover the path predicate alone; only this
//! suite proves the predicate is wired to anything.

#![cfg(feature = "integration")]

use axum::http::StatusCode;
use tankovault_api_test_support::{TestApp, TestConfig};
use tankovault_domain::{AccountStatus, Feature};

/// The harness with the wall up. Rate limiting off, like every other functional suite: a
/// throttle here would read as a refusal by the gate under test.
async fn walled() -> TestApp {
    TestApp::spawn_with(
        TestConfig::new()
            .without_rate_limiting()
            .with_features_enabled(&[Feature::AccountsRequired]),
    )
    .await
}

/// With the wall up, a signed-out caller gets `401 account_required` — its own problem type,
/// not the generic `unauthorized`, because the client has to tell "this deployment is private"
/// apart from "your session ended" to decide between its sign-in screen and an error.
#[tokio::test]
async fn an_anonymous_caller_is_refused_with_its_own_problem_type() {
    let app = walled().await;

    for path in [
        "/v1/series",
        "/v1/tags",
        "/v1/providers",
        "/v1/me/watchlist",
    ] {
        let (status, body) = app.call("GET", path, None, None).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "`{path}` must be behind the wall"
        );
        assert_eq!(
            body["title"], "account_required",
            "`{path}` answered the generic 401; the client cannot act on that"
        );
        assert_eq!(
            body["status"], 401,
            "RFC 9457 requires the body's status member to echo the HTTP one"
        );
    }
}

/// The capability probe is refused *by the wall*, and that refusal is the discovery channel:
/// it is how a signed-out client learns the deployment is private at all. Exempting it would
/// answer `unauthorized` here — indistinguishable from an expired session — and the web app
/// would have no way to know it should show the sign-in screen instead of an error.
#[tokio::test]
async fn the_capability_probe_carries_the_answer_the_client_needs() {
    let app = walled().await;

    let (status, body) = app.call("GET", "/v1/me/capabilities", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["title"], "account_required");
}

/// The way in stays open, or the flag is not a private deployment but a bricked one.
///
/// The sign-in assertion is about *reaching the handler*: bad credentials answer `401
/// unauthorized`, and that token — rather than `account_required` — is what proves the request
/// got past the wall instead of being turned away at it.
#[tokio::test]
async fn the_surface_that_grants_an_account_stays_open() {
    let app = walled().await;

    let (status, body) = app
        .call(
            "POST",
            "/v1/auth/login",
            None,
            Some(serde_json::json!({"login": "nobody", "password": "wrong-password"})),
        )
        .await;
    assert_ne!(
        body["title"], "account_required",
        "sign-in must reach its handler, or nobody on a private deployment can ever sign in"
    );
    assert!(
        status.is_client_error(),
        "the credentials are wrong; the point is only that the wall did not answer"
    );

    // Registering *is* the act of accepting the Terms, so the documents have to be readable by
    // someone who has not registered yet.
    let (status, _) = app.call("GET", "/v1/legal", None, None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the legal index is linked from the register form and must survive the wall"
    );

    // The sign-in screen draws the wordmark, the tagline and the copyright line. Walled, the
    // client falls back to the shipped identity, so a rebranded private deployment would greet
    // every visitor under this project's name.
    let (status, body) = app.call("GET", "/v1/branding", None, None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the sign-in screen is the whole public face of a private deployment; it has to know \
         what that deployment is called"
    );
    assert_eq!(body["name"], "TankoVault");
}

/// An account gets through. The inverse leg: a gate that refused unconditionally would pass
/// every assertion above while making the deployment unusable for the people it is for.
#[tokio::test]
async fn an_account_passes_the_wall() {
    let app = walled().await;
    let user = app.seed_user("reader", &[], AccountStatus::Active).await;
    let token = app.bearer(user);

    let (status, _) = app
        .call("GET", "/v1/me/capabilities", Some(&token), None)
        .await;
    assert_eq!(status, StatusCode::OK);
}

/// A token this deployment did not sign is not an account.
///
/// The gate reads the `Authorization` header itself, so it has its own verification to get
/// wrong; a header-presence check would pass this and hand the whole catalogue to anyone who
/// sends the word "Bearer".
#[tokio::test]
async fn a_forged_bearer_token_is_not_an_account() {
    let app = walled().await;

    for header in ["Bearer not-a-token", "Bearer ", "Basic bm9ib2R5"] {
        let (status, body) = app.call("GET", "/v1/series", Some(header), None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "`{header}` was admitted");
        assert_eq!(body["title"], "account_required", "`{header}` was admitted");
    }
}

/// With the flag at its shipped default the wall is not there at all, and a signed-out caller
/// gets the ordinary refusal from the route's own extractor.
///
/// This is what pins the default: a mounted layer that walled by accident would turn every
/// public deployment private on upgrade, and the flag ships **off** precisely so no deployment
/// changes behaviour by installing this.
#[tokio::test]
async fn the_wall_is_absent_by_default() {
    let app = TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await;

    let (status, body) = app.call("GET", "/v1/me/capabilities", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        body["title"], "unauthorized",
        "with the flag off nothing may answer `account_required`"
    );
}
