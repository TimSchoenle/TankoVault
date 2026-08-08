//! End-to-end HTTP access-control tests, run against the real router and an ephemeral Postgres.
//! Gated behind the `integration` feature because they require Docker.
#![cfg(feature = "integration")]

use axum::http::StatusCode;
use serde_json::json;
use tankovault_api_test_support::TestApp;
use tankovault_db::repo::users::mfa::StepUpMethod;
use tankovault_domain::{AccountStatus, Permission};

/// Seed an administrative account and return the two headers every privileged call now needs.
///
/// A privileged account without a second factor is refused before any capability is consulted,
/// and a *mutating* capability additionally needs a fresh step-up. Both rules live in
/// `AuthUser::require_all` and are pinned by `mfa.rs`; here they are scaffolding, so this helper
/// keeps them out of the assertions that are actually the subject.
async fn admin(
    app: &TestApp,
    username: &str,
    perms: &[Permission],
    status: AccountStatus,
) -> (String, String) {
    let user = app.seed_user(username, perms, status).await;
    (app.bearer(user), app.enrolled_and_elevated(user).await)
}

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

        let (bearer, step_up) = admin(
            &app,
            &format!("holder_{permission}"),
            &[permission],
            AccountStatus::Active,
        )
        .await;
        let (status, _) = app
            .call_elevated("GET", path, Some(&bearer), Some(&step_up), None)
            .await;
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
    let (bearer, step_up) = admin(
        &app,
        "owner",
        &[Permission::SuperUser],
        AccountStatus::Active,
    )
    .await;

    for (permission, path) in gated_read_routes() {
        let (status, _) = app
            .call_elevated("GET", path, Some(&bearer), Some(&step_up), None)
            .await;
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
    let (bearer, step_up) = admin(
        &app,
        "granter",
        &[Permission::UsersPermissions],
        AccountStatus::Active,
    )
    .await;
    let target = app.seed_user("target", &[], AccountStatus::Active).await;

    let (status, _) = app
        .call_elevated(
            "PUT",
            &format!("/v1/admin/users/{}/permissions", target.as_uuid()),
            Some(&bearer),
            Some(&step_up),
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

/// The owner's grant lives on one row of one account, cannot be minted through the API, and dies
/// with the account it sits on — so suspending or erasing the owner ends the deployment's
/// ownership for good.
///
/// The last-administrator guard does not catch this: it waves the action through the moment a
/// second administrator exists, which is why the caller here is given `users.permissions` too.
/// Without a refusal of its own, an administrator could erase the owner and be promoted into the
/// vacancy by the next boot's reconciliation.
#[tokio::test]
async fn an_administrator_cannot_suspend_or_erase_the_deployment_owner() {
    let app = TestApp::spawn().await;
    let (bearer, step_up) = admin(
        &app,
        "deputy",
        &[
            Permission::UsersWrite,
            Permission::UsersDelete,
            Permission::UsersPermissions,
        ],
        AccountStatus::Active,
    )
    .await;
    let owner = app
        .seed_user("owner", &[Permission::SuperUser], AccountStatus::Active)
        .await;

    let (status, _) = app
        .call_elevated(
            "POST",
            &format!("/v1/admin/users/{}/status", owner.as_uuid()),
            Some(&bearer),
            Some(&step_up),
            Some(json!({ "status": "suspended", "reason": "because I can" })),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "the owner cannot be suspended"
    );

    let (status, _) = app
        .call_elevated(
            "DELETE",
            &format!("/v1/admin/users/{}", owner.as_uuid()),
            Some(&bearer),
            Some(&step_up),
            Some(json!({ "confirm_username": "owner" })),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "the owner cannot be erased"
    );

    let principal = tankovault_db::repo::permissions::resolve(&app.db.pool, owner)
        .await
        .expect("resolve")
        .expect("owner exists");
    assert_eq!(principal.status, AccountStatus::Active);
    assert!(principal.permissions.is_super_user());
}

/// The catalogue is what the editor renders its checklist from, so a super user entry there
/// would be a checkbox whose every submission the write path rejects.
#[tokio::test]
async fn the_permission_catalogue_does_not_offer_the_super_user() {
    let app = TestApp::spawn().await;
    let (bearer, step_up) = admin(
        &app,
        "cataloguer",
        &[Permission::UsersRead],
        AccountStatus::Active,
    )
    .await;

    let (status, body) = app
        .call_elevated(
            "GET",
            "/v1/admin/permissions",
            Some(&bearer),
            Some(&step_up),
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

/// A refused privilege escalation is recorded, not merely rejected — and the record names the
/// capability that was missing.
///
/// The caller is enrolled and elevated deliberately. Without a second factor the authorization
/// funnel refuses at its *first* gate and never reaches the capability check, so the denial
/// would name no missing permission and this test would be asserting a different refusal than
/// the one it describes.
#[tokio::test]
async fn a_denied_call_emits_an_authz_denied_audit_event() {
    let app = TestApp::spawn().await;
    let nobody = app.seed_user("auditless", &[], AccountStatus::Active).await;
    let step_up = app.enrolled_and_elevated(nobody).await;

    let (status, _) = app
        .call_elevated(
            "GET",
            "/v1/admin/stats",
            Some(&app.bearer(nobody)),
            Some(&step_up),
            None,
        )
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
    // Filing and withdrawing are both behind a step-up now. Neither account has a factor, so a
    // password-earned grant is the strongest proof each can offer — and the one the fallback
    // exists for.
    let owner_step_up = app.step_up(owner, StepUpMethod::Password).await;
    let stranger_step_up = app.step_up(stranger, StepUpMethod::Password).await;

    let (status, body) = app
        .call_elevated(
            "POST",
            "/v1/me/privacy/requests",
            Some(&app.bearer(owner)),
            Some(&owner_step_up),
            Some(json!({ "kind": "access" })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let id = body["id"].as_str().expect("request id").to_owned();

    // Ownership scoping turns this into a 404, same as a nonexistent request.
    let (status, _) = app
        .call_elevated(
            "DELETE",
            &format!("/v1/me/privacy/requests/{id}"),
            Some(&app.bearer(stranger)),
            Some(&stranger_step_up),
            None,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a stranger must not be able to cancel another user's request"
    );

    let (status, _) = app
        .call_elevated(
            "DELETE",
            &format!("/v1/me/privacy/requests/{id}"),
            Some(&app.bearer(owner)),
            Some(&owner_step_up),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}
