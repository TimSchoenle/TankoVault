//! Access-control tests against a real, migrated Postgres.
//!
//! Gated behind the `integration` feature (requires Docker).
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

/// The bug this pins: `LIKE` on a concatenated pattern resolves to case-sensitive `text`
/// even though `username`/`email` are `citext`, so searching `alice` missed `Alice`. Fixed
/// with `ILIKE`.
#[tokio::test]
async fn the_admin_directory_search_is_case_insensitive_on_both_columns() {
    let db = TestDb::spawn().await;
    db.seed_user("Alice", &[], AccountStatus::Active).await;
    db.seed_user("bob", &[], AccountStatus::Active).await;

    let found = |search: &str| {
        let pool = db.pool.clone();
        let search = search.to_owned();
        async move {
            user_admin::directory(&pool, &search, 50, 0)
                .await
                .expect("directory")
        }
    };

    for search in ["alice", "Alice", "ALICE", "lic"] {
        let page = found(search).await;
        assert_eq!(
            page.users
                .iter()
                .map(|r| r.username.as_str())
                .collect::<Vec<_>>(),
            vec!["Alice"],
            "search={search:?} must find the account however it was capitalised"
        );
        assert_eq!(
            page.total, 1,
            "search={search:?}: the total must match the rows"
        );
    }

    // Matches through `email`, the second copy of the predicate.
    let page = found("ALICE@EXAMPLE.TEST").await;
    assert_eq!(
        page.total, 1,
        "the email predicate must be case-insensitive too"
    );

    // An empty search is every account, not a pattern match against nothing.
    let all = found("").await;
    assert_eq!(all.total, 2);
}
