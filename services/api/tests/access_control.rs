//! End-to-end HTTP access-control tests.
//!
//! These drive the *real* router (the `AuthUser` extractor, the middleware stack, and every
//! handler's `require`) in-process via the shared harness, against an ephemeral Postgres. They
//! are the automated replacement for the manual smoke script's authorization checks: a
//! privilege-escalation regression fails here instead of reaching production.
//!
//! Opt-in: gated behind the `integration` feature because they require Docker.
#![cfg(feature = "integration")]

use axum::http::StatusCode;
use serde_json::json;
use tankovault_domain::{AccountStatus, Permission};
use tankovault_test_support::TestApp;

/// Every permission-gated **read** endpoint, paired with the capability it requires. Read
/// endpoints are used for the matrix because a `2xx` is cleanly assertable against an empty
/// schema without a route-specific request body; the mutating routes are exercised by the
/// manual smoke script and by targeted tests. All of these are ungated by feature flags, so a
/// missing token yields `401` (not a disabled-feature `404`).
fn gated_read_routes() -> Vec<(Permission, &'static str)> {
    vec![
        (Permission::FlagsRead, "/v1/admin/feature-flags"),
        (Permission::MergeRead, "/v1/admin/merge-candidates"),
        (Permission::SystemStats, "/v1/admin/stats"),
        (Permission::AuditRead, "/v1/admin/audit"),
        (Permission::ProvidersRead, "/v1/admin/providers"),
        (Permission::UsersRead, "/v1/admin/users"),
        (Permission::ScansRead, "/v1/admin/scans"),
        (Permission::SyncAdminRead, "/v1/admin/sync/accounts"),
    ]
}

#[tokio::test]
async fn permission_gated_routes_enforce_the_full_matrix() {
    let app = TestApp::spawn().await;

    // A caller holding *no* capability — used for every `403` case.
    let nobody = app.seed_user("nobody", &[], AccountStatus::Active).await;
    let nobody_bearer = app.bearer(nobody);

    for (permission, path) in gated_read_routes() {
        // 1. Unauthenticated: no token at all.
        let (status, _) = app.call("GET", path, None, None).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "GET {path} without a token must be 401"
        );

        // 2. Authenticated but unprivileged: a valid token lacking the capability.
        let (status, _) = app.call("GET", path, Some(&nobody_bearer), None).await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "GET {path} without {permission} must be 403"
        );

        // 3. Authorized: a token holding exactly the required capability.
        let holder = app
            .seed_user(
                &format!("holder_{permission}"),
                &[permission],
                AccountStatus::Active,
            )
            .await;
        let (status, _) = app.call("GET", path, Some(&app.bearer(holder)), None).await;
        assert!(
            status.is_success(),
            "GET {path} with {permission} must succeed, got {status}"
        );
    }
}

#[tokio::test]
async fn a_denied_call_emits_an_authz_denied_audit_event() {
    let app = TestApp::spawn().await;
    let nobody = app.seed_user("auditless", &[], AccountStatus::Active).await;

    // A caller without `system.stats` hits the stats endpoint: it must be refused *and*
    // recorded, because a refused privilege escalation is the most interesting thing the audit
    // trail can hold.
    let (status, _) = app
        .call("GET", "/v1/admin/stats", Some(&app.bearer(nobody)), None)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let denials = app.audit.denials();
    assert_eq!(denials.len(), 1, "exactly one denial should be recorded");
    let event = &denials[0];
    assert_eq!(event.action, "authz.denied");
    assert_eq!(event.actor, Some(nobody));

    // The event must name the missing capability, so an incident responder sees *what* was
    // attempted, not merely that something was refused.
    let missing = event.detail["missing"]
        .as_array()
        .expect("missing is an array");
    assert!(
        missing.iter().any(|m| m == "system.stats"),
        "the denial must name the missing capability, got {:?}",
        event.detail
    );
}

#[tokio::test]
async fn a_suspended_account_is_rejected_before_any_capability_check() {
    let app = TestApp::spawn().await;

    // A suspended account that still holds a capability: suspension must win, and the refusal
    // must be distinguishable from an ordinary "insufficient privileges".
    let suspended = app
        .seed_user(
            "banned",
            &[Permission::SystemStats],
            AccountStatus::Suspended,
        )
        .await;

    let (status, body) = app
        .call("GET", "/v1/admin/stats", Some(&app.bearer(suspended)), None)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        body["title"], "account_suspended",
        "a suspended account must be told it is suspended, not that it lacks a permission"
    );
}

#[tokio::test]
async fn a_privacy_request_cannot_be_cancelled_by_another_user() {
    let app = TestApp::spawn().await;
    let owner = app.seed_user("subject", &[], AccountStatus::Active).await;
    let stranger = app.seed_user("meddler", &[], AccountStatus::Active).await;

    // The owner files a request.
    let (status, body) = app
        .call(
            "POST",
            "/v1/me/privacy/requests",
            Some(&app.bearer(owner)),
            Some(json!({ "kind": "access" })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let id = body["id"].as_str().expect("request id").to_owned();

    // A stranger holding the id is not authority to cancel it: ownership scoping turns it into
    // a `404`, the same as if the request did not exist for them.
    let (status, _) = app
        .call(
            "DELETE",
            &format!("/v1/me/privacy/requests/{id}"),
            Some(&app.bearer(stranger)),
            None,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a stranger must not be able to cancel another user's request"
    );

    // The owner can.
    let (status, _) = app
        .call(
            "DELETE",
            &format!("/v1/me/privacy/requests/{id}"),
            Some(&app.bearer(owner)),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}
