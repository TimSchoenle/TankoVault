//! DB-backed access-control tests for the repository guard rails.
//!
//! These run the real SQL in `tankovault_db::repo` against a freshly-migrated, ephemeral
//! Postgres (see `tankovault_test_support`). They pin the behaviour the authorization layer
//! depends on: exact permission resolution, the suspended-status path, the last-active-holder
//! guard, suspend/reinstate, and `/me/*` ownership scoping.
//!
//! Opt-in: gated behind the `integration` feature because they require Docker. The default
//! `cargo test -p tankovault-db` stays fast and DB-free.
#![cfg(feature = "integration")]

use tankovault_db::repo::{gdpr, permissions, user_admin};
use tankovault_domain::{AccountStatus, Permission, PermissionSet};
use tankovault_test_support::TestDb;

#[tokio::test]
async fn resolve_returns_exact_live_grants_and_active_status() {
    let db = TestDb::spawn().await;
    let uid = db
        .seed_user(
            "operator",
            &[Permission::ScansRead, Permission::ScansRun],
            AccountStatus::Active,
        )
        .await;

    let principal = permissions::resolve(&db.pool, uid)
        .await
        .expect("resolve")
        .expect("principal exists");

    assert_eq!(principal.status, AccountStatus::Active);
    assert!(principal.status.may_authenticate());
    assert!(principal.permissions.has(Permission::ScansRead));
    assert!(principal.permissions.has(Permission::ScansRun));
    // Resolution returns *exactly* the live grant set — nothing it was never granted.
    assert!(!principal.permissions.has(Permission::UsersPermissions));
    assert_eq!(principal.permissions.len(), 2);
}

#[tokio::test]
async fn resolve_reports_suspension_so_the_extractor_can_reject_it() {
    let db = TestDb::spawn().await;
    let uid = db
        .seed_user(
            "suspended",
            &[Permission::ScansRead],
            AccountStatus::Suspended,
        )
        .await;

    let principal = permissions::resolve(&db.pool, uid)
        .await
        .expect("resolve")
        .expect("principal exists");

    assert_eq!(principal.status, AccountStatus::Suspended);
    // The whole point of resolving status alongside grants: a suspended account is refused
    // before any capability is consulted, even though it still holds grants.
    assert!(!principal.status.may_authenticate());
    assert!(principal.permissions.has(Permission::ScansRead));
}

#[tokio::test]
async fn resolve_is_none_for_a_token_outliving_its_account() {
    let db = TestDb::spawn().await;
    // A random id that was never created: a valid signature over a deleted account.
    let ghost = tankovault_domain::UserId::new();
    assert!(
        permissions::resolve(&db.pool, ghost)
            .await
            .expect("resolve")
            .is_none()
    );
}

#[tokio::test]
async fn other_active_holders_protects_the_last_holder_of_a_critical_capability() {
    let db = TestDb::spawn().await;
    let admin = db
        .seed_user(
            "root",
            &[Permission::UsersPermissions],
            AccountStatus::Active,
        )
        .await;

    // Nobody else holds it: removing it from `admin` would strand the deployment.
    let others = permissions::other_active_holders(&db.pool, Permission::UsersPermissions, admin)
        .await
        .expect("count holders");
    assert_eq!(
        others, 0,
        "the last holder must have no other active holders"
    );

    // A second active holder makes removal safe.
    let second = db
        .seed_user(
            "deputy",
            &[Permission::UsersPermissions],
            AccountStatus::Active,
        )
        .await;
    let others = permissions::other_active_holders(&db.pool, Permission::UsersPermissions, admin)
        .await
        .expect("count holders");
    assert_eq!(others, 1, "the deputy is another active holder");

    // A *suspended* holder is not a recovery path, so it must not be counted.
    user_admin::set_status(&db.pool, second, AccountStatus::Suspended, Some("test"))
        .await
        .expect("suspend deputy");
    let others = permissions::other_active_holders(&db.pool, Permission::UsersPermissions, admin)
        .await
        .expect("count holders");
    assert_eq!(others, 0, "a suspended holder cannot rescue the deployment");
}

#[tokio::test]
async fn replace_reports_the_precise_diff_and_takes_effect_immediately() {
    let db = TestDb::spawn().await;
    let granter = db
        .seed_user(
            "granter",
            &[Permission::UsersPermissions],
            AccountStatus::Active,
        )
        .await;
    let target = db
        .seed_user("target", &[Permission::ScansRead], AccountStatus::Active)
        .await;

    let desired: PermissionSet = [Permission::ScansRead, Permission::MergeRead]
        .into_iter()
        .collect();

    let mut conn = db.pool.acquire().await.expect("acquire");
    let diff = permissions::replace(&mut conn, target, &desired, granter)
        .await
        .expect("replace grants");
    drop(conn);

    assert_eq!(diff.added, vec!["merge.read".to_owned()]);
    assert!(
        diff.removed.is_empty(),
        "scans.read was already held and is unchanged"
    );

    let principal = permissions::resolve(&db.pool, target)
        .await
        .expect("resolve")
        .expect("exists");
    assert!(principal.permissions.has(Permission::ScansRead));
    assert!(principal.permissions.has(Permission::MergeRead));
    assert_eq!(principal.permissions.len(), 2);
}

#[tokio::test]
async fn set_status_suspends_and_reinstates_round_trip() {
    let db = TestDb::spawn().await;
    let uid = db
        .seed_user("wobbly", &[Permission::ScansRead], AccountStatus::Active)
        .await;

    user_admin::set_status(&db.pool, uid, AccountStatus::Suspended, Some("abuse"))
        .await
        .expect("suspend");
    let suspended = permissions::resolve(&db.pool, uid).await.unwrap().unwrap();
    assert_eq!(suspended.status, AccountStatus::Suspended);
    // Suspension is reversible and clears nothing else: grants survive.
    assert!(suspended.permissions.has(Permission::ScansRead));

    user_admin::set_status(&db.pool, uid, AccountStatus::Active, None)
        .await
        .expect("reinstate");
    let reinstated = permissions::resolve(&db.pool, uid).await.unwrap().unwrap();
    assert_eq!(reinstated.status, AccountStatus::Active);
    assert!(reinstated.status.may_authenticate());
}

#[tokio::test]
async fn cancel_own_is_scoped_to_the_owner() {
    let db = TestDb::spawn().await;
    let owner = db.seed_user("owner", &[], AccountStatus::Active).await;
    let stranger = db.seed_user("stranger", &[], AccountStatus::Active).await;

    let request = gdpr::create(&db.pool, owner, gdpr::RequestKind::Access, None)
        .await
        .expect("file request");

    // A stranger holding the id is not authority to cancel it: the owner scoping is the guard.
    let cancelled_by_stranger = gdpr::cancel_own(&db.pool, request.id, stranger)
        .await
        .expect("attempt cancel");
    assert!(
        !cancelled_by_stranger,
        "a stranger must not cancel another user's request"
    );

    // The owner can.
    let cancelled_by_owner = gdpr::cancel_own(&db.pool, request.id, owner)
        .await
        .expect("cancel own");
    assert!(cancelled_by_owner, "the owner may cancel their own request");
}
