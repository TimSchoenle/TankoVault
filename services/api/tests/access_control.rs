//! Tests HTTP access control end to end, against the real router and an ephemeral Postgres.
//! Gated behind the `integration` feature because they require Docker.
#![cfg(feature = "integration")]

use axum::http::StatusCode;
use serde_json::json;
use tankovault_api_test_support::TestApp;
use tankovault_db::repo::users::mfa::StepUpMethod;
use tankovault_domain::{AccountStatus, Permission};

/// Seeds an administrative account and returns the two headers every privileged call needs.
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
/// deployment owner out of the console they own, with a permission editor that cannot grant
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
/// with the account it sits on, so suspending or erasing the owner ends the deployment's
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

/// The bug: `users.sessions` alone let an administrator revoke every session of the owner or
/// of a broader administrator, as often as they liked, signing them out for good in practice.
/// Both are now refused, while an ordinary account can still be signed out.
#[tokio::test]
async fn an_administrator_cannot_sign_out_an_account_that_outranks_them() {
    let app = TestApp::spawn().await;
    let (bearer, step_up) = admin(
        &app,
        "doorman",
        &[Permission::UsersSessions],
        AccountStatus::Active,
    )
    .await;
    let owner = app
        .seed_user("owner", &[Permission::SuperUser], AccountStatus::Active)
        .await;
    let peer = app
        .seed_user(
            "peer",
            &[Permission::UsersSessions, Permission::UsersPermissions],
            AccountStatus::Active,
        )
        .await;
    let reader = app.seed_user("reader", &[], AccountStatus::Active).await;

    for (target, name) in [(owner, "owner"), (peer, "peer")] {
        let (status, _) = app
            .call_elevated(
                "POST",
                &format!("/v1/admin/users/{}/revoke-sessions", target.as_uuid()),
                Some(&bearer),
                Some(&step_up),
                None,
            )
            .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{name} outranks the caller"
        );
    }

    let (status, _) = app
        .call_elevated(
            "POST",
            &format!("/v1/admin/users/{}/revoke-sessions", reader.as_uuid()),
            Some(&bearer),
            Some(&step_up),
            None,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "an ordinary account can be signed out"
    );
}

/// The bug: fulfilling an erasure request ran the erasure cascade with neither guard that
/// administrative deletion applies, so `privacy.write` plus `users.delete` could erase the
/// owner, or the only account able to grant permissions, by way of the privacy queue.
#[tokio::test]
async fn an_erasure_request_cannot_erase_the_owner_or_the_last_administrator() {
    use tankovault_db::repo::gdpr::{self, RequestKind, RequestStatus};

    let app = TestApp::spawn().await;
    let (bearer, step_up) = admin(
        &app,
        "clerk",
        &[Permission::PrivacyWrite, Permission::UsersDelete],
        AccountStatus::Active,
    )
    .await;

    // Seeded before the owner, whose grant would otherwise count as another administrator.
    let keeper = app
        .seed_user(
            "keeper",
            &[Permission::UsersPermissions],
            AccountStatus::Active,
        )
        .await;
    let keeper_request = gdpr::create(&app.db.pool, keeper, RequestKind::Erasure, None)
        .await
        .expect("file request")
        .id;
    let (status, _) = app
        .call_elevated(
            "POST",
            &format!("/v1/admin/privacy/requests/{keeper_request}/fulfil-erasure"),
            Some(&bearer),
            Some(&step_up),
            Some(json!({ "confirm_username": "keeper" })),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "the last administrator cannot be erased"
    );

    let owner = app
        .seed_user("owner", &[Permission::SuperUser], AccountStatus::Active)
        .await;
    let owner_request = gdpr::create(&app.db.pool, owner, RequestKind::Erasure, None)
        .await
        .expect("file request")
        .id;
    let (status, _) = app
        .call_elevated(
            "POST",
            &format!("/v1/admin/privacy/requests/{owner_request}/fulfil-erasure"),
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

    for (subject, request) in [(keeper, keeper_request), (owner, owner_request)] {
        tankovault_db::repo::users::get(&app.db.pool, subject)
            .await
            .expect("the subject still exists");
        let row = gdpr::get_admin(&app.db.pool, request)
            .await
            .expect("request exists");
        assert_eq!(
            row.request.status,
            RequestStatus::Pending,
            "a refused erasure is not recorded as done"
        );
    }
}

/// The bug: the subject export never looked at the request's status, so one access request,
/// once answered, let `privacy.export` re-read the subject's whole record indefinitely.
#[tokio::test]
async fn a_resolved_access_request_cannot_be_exported_again() {
    use tankovault_db::repo::gdpr::{self, RequestKind, RequestStatus};

    let app = TestApp::spawn().await;
    let (bearer, step_up) = admin(
        &app,
        "clerk",
        &[Permission::PrivacyExport],
        AccountStatus::Active,
    )
    .await;
    let subject = app.seed_user("subject", &[], AccountStatus::Active).await;
    let request = gdpr::create(&app.db.pool, subject, RequestKind::Access, None)
        .await
        .expect("file request")
        .id;
    let path = format!("/v1/admin/privacy/requests/{request}/export");

    let (status, _) = app
        .call_elevated("GET", &path, Some(&bearer), Some(&step_up), None)
        .await;
    assert_eq!(status, StatusCode::OK, "an open request can be exported");

    assert!(
        gdpr::resolve(
            &app.db.pool,
            request,
            RequestStatus::Completed,
            None,
            Some("sent")
        )
        .await
        .expect("resolve")
    );
    let (status, _) = app
        .call_elevated("GET", &path, Some(&bearer), Some(&step_up), None)
        .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a resolved request is no longer a reason to disclose"
    );
}

/// An email address is the recovery channel, so redirecting it is a password reset away from
/// taking the account.
///
/// The bug: `users.write` alone let an administrator rewrite the owner's or a broader
/// administrator's address, with no guard at all. It is now refused on any account holding a
/// permission the caller lacks, while an ordinary account stays editable.
#[tokio::test]
async fn an_administrator_cannot_redirect_the_email_of_an_account_that_outranks_them() {
    let app = TestApp::spawn().await;
    let (bearer, step_up) = admin(
        &app,
        "editor",
        &[Permission::UsersWrite],
        AccountStatus::Active,
    )
    .await;
    let owner = app
        .seed_user("owner", &[Permission::SuperUser], AccountStatus::Active)
        .await;
    let peer = app
        .seed_user(
            "peer",
            &[Permission::UsersWrite, Permission::UsersPermissions],
            AccountStatus::Active,
        )
        .await;
    let reader = app.seed_user("reader", &[], AccountStatus::Active).await;

    for (target, name) in [(owner, "owner"), (peer, "peer")] {
        let before = tankovault_db::repo::users::get(&app.db.pool, target)
            .await
            .expect("target exists");
        let (status, _) = app
            .call_elevated(
                "PATCH",
                &format!("/v1/admin/users/{}", target.as_uuid()),
                Some(&bearer),
                Some(&step_up),
                Some(json!({ "email": "attacker@example.test" })),
            )
            .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{name} outranks the caller"
        );
        let after = tankovault_db::repo::users::get(&app.db.pool, target)
            .await
            .expect("target exists");
        assert_eq!(after.email, before.email, "{name}'s address must not move");
    }

    let (status, _) = app
        .call_elevated(
            "PATCH",
            &format!("/v1/admin/users/{}", reader.as_uuid()),
            Some(&bearer),
            Some(&step_up),
            Some(json!({ "email": "reader-new@example.test" })),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "an account without grants stays editable"
    );
}

/// `users.permissions` must not be every permission in disguise.
///
/// The bug: an administrator holding only `users.permissions` could grant any capability to an
/// account they also controlled, and strip grants from a broader administrator. Additions are
/// now capped at the caller's own set, and an account holding more than the caller is refused.
#[tokio::test]
async fn an_administrator_can_grant_only_what_they_hold() {
    let app = TestApp::spawn().await;
    let (bearer, step_up) = admin(
        &app,
        "granter",
        &[Permission::UsersPermissions, Permission::UsersRead],
        AccountStatus::Active,
    )
    .await;
    let sock = app.seed_user("sock", &[], AccountStatus::Active).await;
    let senior = app
        .seed_user(
            "senior",
            &[Permission::UsersPermissions, Permission::AuditRead],
            AccountStatus::Active,
        )
        .await;

    let put = |target: tankovault_domain::UserId, perms: serde_json::Value| {
        let bearer = bearer.clone();
        let step_up = step_up.clone();
        let app = &app;
        async move {
            app.call_elevated(
                "PUT",
                &format!("/v1/admin/users/{}/permissions", target.as_uuid()),
                Some(&bearer),
                Some(&step_up),
                Some(json!({ "permissions": perms })),
            )
            .await
            .0
        }
    };

    assert_eq!(
        put(sock, json!(["users.read", "audit.read"])).await,
        StatusCode::BAD_REQUEST,
        "audit.read is beyond the caller"
    );
    let held = tankovault_db::repo::permissions::resolve(&app.db.pool, sock)
        .await
        .expect("resolve")
        .expect("sock exists");
    assert!(held.permissions.is_empty(), "refused whole, not in part");

    assert_eq!(
        put(sock, json!(["users.read"])).await,
        StatusCode::OK,
        "a permission the caller holds is grantable"
    );

    assert_eq!(
        put(senior, json!(["audit.read"])).await,
        StatusCode::BAD_REQUEST,
        "an account holding more than the caller cannot be edited"
    );
    let held = tankovault_db::repo::permissions::resolve(&app.db.pool, senior)
        .await
        .expect("resolve")
        .expect("senior exists");
    assert!(held.permissions.has(Permission::UsersPermissions));
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

/// A refused privilege escalation is recorded, not merely rejected, and the record names the
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
    // Filing and withdrawing are both behind a step-up. Neither account has a factor, so a
    // password-earned grant is the strongest proof each can offer, and the one the fallback
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
