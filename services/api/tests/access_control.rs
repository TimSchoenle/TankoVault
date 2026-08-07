//! End-to-end HTTP access-control tests, run against the real router and an ephemeral Postgres.
//! Gated behind the `integration` feature because they require Docker.
#![cfg(feature = "integration")]

use axum::http::StatusCode;
use serde_json::json;
use tankovault_api_test_support::TestApp;
use tankovault_domain::{AccountStatus, Permission};

/// Permission-gated read endpoints paired with the capability each requires.
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

    let nobody = app.seed_user("nobody", &[], AccountStatus::Active).await;
    let nobody_bearer = app.bearer(nobody);

    for (permission, path) in gated_read_routes() {
        let (status, _) = app.call("GET", path, None, None).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "GET {path} without a token must be 401"
        );

        let (status, _) = app.call("GET", path, Some(&nobody_bearer), None).await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "GET {path} without {permission} must be 403"
        );

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

/// One grant, every door. The super user holds a single stored token and no enumerated
/// capability, so a build that lost the implication in `PermissionSet::has` would 403 the
/// deployment owner out of the console they own — with a permission editor that cannot grant
/// them their way back in.
#[tokio::test]
async fn the_super_user_passes_every_capability_check() {
    let app = TestApp::spawn().await;
    let owner = app
        .seed_user("owner", &[Permission::SuperUser], AccountStatus::Active)
        .await;
    let bearer = app.bearer(owner);

    for (permission, path) in gated_read_routes() {
        let (status, _) = app.call("GET", path, Some(&bearer), None).await;
        assert!(
            status.is_success(),
            "GET {path} needs {permission}, which the super user holds implicitly, got {status}"
        );
    }
}

/// The super user is minted by the installer and by nothing else. `users.permissions` is
/// otherwise total power, so without this refusal any administrator could promote an account
/// past every capability the enum will ever gain.
#[tokio::test]
async fn the_super_user_grant_cannot_be_handed_out_by_an_administrator() {
    let app = TestApp::spawn().await;
    let admin = app
        .seed_user(
            "granter",
            &[Permission::UsersPermissions],
            AccountStatus::Active,
        )
        .await;
    let target = app.seed_user("target", &[], AccountStatus::Active).await;

    let (status, _) = app
        .call(
            "PUT",
            &format!("/v1/admin/users/{}/permissions", target.as_uuid()),
            Some(&app.bearer(admin)),
            Some(json!({ "permissions": ["system.superuser", "users.read"] })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Refused whole, not partially applied: the accompanying grant must not land either.
    let principal = tankovault_db::repo::permissions::resolve(&app.db.pool, target)
        .await
        .expect("resolve")
        .expect("target exists");
    assert!(!principal.permissions.is_super_user());
    assert!(principal.permissions.is_empty());
}

/// The catalogue is what the editor renders its checklist from, so a super user entry there
/// would be a checkbox whose every submission the write path rejects.
#[tokio::test]
async fn the_permission_catalogue_does_not_offer_the_super_user() {
    let app = TestApp::spawn().await;
    let reader = app
        .seed_user(
            "cataloguer",
            &[Permission::UsersRead],
            AccountStatus::Active,
        )
        .await;

    let (status, body) = app
        .call(
            "GET",
            "/v1/admin/permissions",
            Some(&app.bearer(reader)),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let listed = serde_json::to_string(&body).expect("serialise catalogue");
    assert!(
        !listed.contains("system.superuser"),
        "neither the permission list nor a preset may name the super user: {listed}"
    );
}

#[tokio::test]
async fn a_denied_call_emits_an_authz_denied_audit_event() {
    let app = TestApp::spawn().await;
    let nobody = app.seed_user("auditless", &[], AccountStatus::Active).await;

    // A refused privilege escalation must be recorded, not just rejected.
    let (status, _) = app
        .call("GET", "/v1/admin/stats", Some(&app.bearer(nobody)), None)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let denials = app.audit.denials();
    assert_eq!(denials.len(), 1, "exactly one denial should be recorded");
    let event = &denials[0];
    assert_eq!(event.action, "authz.denied");
    assert_eq!(event.actor, Some(nobody));

    // Denials must name the missing capability, not just that something was refused.
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

    // Suspension must be checked before capability, and reported distinctly from a 403.
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

    // Ownership scoping turns this into a 404, same as a nonexistent request.
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
