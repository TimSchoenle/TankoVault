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

/// The permission editor's catalogue does not list `system.superuser`, so *every* checklist
/// submit omits it. A plain whole-set diff would therefore revoke the deployment owner's grant
/// the first time anyone edited their other permissions — and no API path can put it back.
#[tokio::test]
async fn a_permission_edit_that_omits_the_super_user_grant_leaves_it_in_place() {
    let db = TestDb::spawn().await;
    let granter = db
        .seed_user(
            "granter",
            &[Permission::UsersPermissions],
            AccountStatus::Active,
        )
        .await;
    let owner = db
        .seed_user(
            "owner",
            &[Permission::SuperUser, Permission::ScansRead],
            AccountStatus::Active,
        )
        .await;

    let desired: PermissionSet = [Permission::MergeRead].into_iter().collect();
    let mut conn = db.pool.acquire().await.expect("acquire");
    let diff = permissions::replace(&mut conn, owner, &desired, granter)
        .await
        .expect("replace grants");
    drop(conn);

    assert_eq!(diff.added, vec!["merge.read".to_owned()]);
    assert_eq!(
        diff.removed,
        vec!["scans.read".to_owned()],
        "only the enumerable grant may be revoked here"
    );

    let principal = permissions::resolve(&db.pool, owner)
        .await
        .expect("resolve")
        .expect("exists");
    assert!(principal.permissions.is_super_user());
    assert!(principal.permissions.has(Permission::MergeRead));
}

/// The whole premise of the grant: it is the installer's, once. A second one would be a second
/// account holding every future capability, granted by whoever ran the job last.
#[tokio::test]
async fn only_the_deployments_first_account_can_claim_the_super_user() {
    let db = TestDb::spawn().await;
    let owner = db.seed_user("owner", &[], AccountStatus::Active).await;

    assert!(
        permissions::claim_super_user(&db.pool, owner)
            .await
            .expect("claim"),
        "the only account in the database becomes the super user"
    );
    assert!(
        !permissions::claim_super_user(&db.pool, owner)
            .await
            .expect("re-claim"),
        "re-running the install job changes nothing"
    );

    let second = db.seed_user("second", &[], AccountStatus::Active).await;
    assert!(
        !permissions::claim_super_user(&db.pool, second)
            .await
            .expect("claim by a later account"),
        "seeding a second administrator must not promote it"
    );
    // And not by writing the row directly either: the partial unique index is what makes the
    // rule hold for any path that reaches the table, including a migration.
    assert!(
        permissions::grant(&db.pool, second, Permission::SuperUser, None)
            .await
            .is_err(),
        "the database must refuse a second super user"
    );

    assert!(
        permissions::resolve(&db.pool, second)
            .await
            .expect("resolve")
            .expect("exists")
            .permissions
            .is_empty()
    );
}

/// Make `user` look older than everything else seeded in the test.
///
/// `ensure_super_user` orders by `created_at`, and accounts seeded back to back are microseconds
/// apart — close enough that asserting on "the earliest" without saying which one is earliest
/// would pin the clock rather than the rule.
async fn backdate(pool: &tankovault_db::PgPool, user: tankovault_domain::UserId) {
    sqlx::query("UPDATE users SET created_at = created_at - interval '1 day' WHERE id = $1")
        .bind(user.as_uuid())
        .execute(pool)
        .await
        .expect("backdate");
}

/// A deployment can end up with no super user at all: accounts registered before the seed job
/// ran (so the installer's claim found the database populated and did nothing), or the owner was
/// erased. Neither state is recoverable by hand — the grant is unforgeable through the API — so
/// the deployment would keep serving with nobody holding the capabilities a later release adds.
#[tokio::test]
async fn ensure_super_user_promotes_the_earliest_active_administrator_when_there_is_none() {
    let db = TestDb::spawn().await;
    let reader = db.seed_user("reader", &[], AccountStatus::Active).await;
    let elder = db
        .seed_user(
            "elder",
            &[Permission::UsersPermissions],
            AccountStatus::Active,
        )
        .await;
    db.seed_user(
        "younger",
        &[Permission::UsersPermissions],
        AccountStatus::Active,
    )
    .await;
    // The oldest account in the deployment holds nothing: ownership follows the capability, not
    // the registration date, or whoever signed up first on a later-bootstrapped server would get
    // the deployment.
    backdate(&db.pool, reader).await;
    backdate(&db.pool, reader).await;
    backdate(&db.pool, elder).await;

    let promoted = permissions::ensure_super_user(&db.pool)
        .await
        .expect("reconcile");
    assert_eq!(promoted, Some(elder));
    assert!(
        permissions::resolve(&db.pool, elder)
            .await
            .expect("resolve")
            .expect("exists")
            .permissions
            .is_super_user()
    );

    assert_eq!(
        permissions::ensure_super_user(&db.pool)
            .await
            .expect("reconcile again"),
        None,
        "an owned deployment is left alone, so every boot after the first is a no-op"
    );
}

/// The grant is single-slot and cannot be moved once written, so spending it on an account that
/// cannot sign in would leave the deployment permanently unowned in a way that looks fixed.
#[tokio::test]
async fn ensure_super_user_skips_a_suspended_candidate_and_waits_for_a_real_one() {
    let db = TestDb::spawn().await;

    assert_eq!(
        permissions::ensure_super_user(&db.pool)
            .await
            .expect("reconcile an empty deployment"),
        None,
        "nobody administers permissions yet, so there is no candidate to promote"
    );

    let suspended = db
        .seed_user(
            "suspended-elder",
            &[Permission::UsersPermissions],
            AccountStatus::Suspended,
        )
        .await;
    backdate(&db.pool, suspended).await;
    let active = db
        .seed_user(
            "active-deputy",
            &[Permission::UsersPermissions],
            AccountStatus::Active,
        )
        .await;

    assert_eq!(
        permissions::ensure_super_user(&db.pool)
            .await
            .expect("reconcile"),
        Some(active),
        "the earliest candidate is suspended, so the earliest usable one takes ownership"
    );
}

/// Reconciliation fills an absence; it never re-decides ownership. An owner who is suspended, or
/// simply younger than some administrator, still owns the deployment.
#[tokio::test]
async fn ensure_super_user_never_displaces_an_existing_owner() {
    let db = TestDb::spawn().await;
    let elder = db
        .seed_user(
            "elder",
            &[Permission::UsersPermissions],
            AccountStatus::Active,
        )
        .await;
    backdate(&db.pool, elder).await;
    let owner = db
        .seed_user("owner", &[Permission::SuperUser], AccountStatus::Suspended)
        .await;

    assert_eq!(
        permissions::ensure_super_user(&db.pool)
            .await
            .expect("reconcile"),
        None
    );
    assert!(
        !permissions::resolve(&db.pool, elder)
            .await
            .expect("resolve")
            .expect("exists")
            .permissions
            .is_super_user()
    );
    assert!(
        permissions::resolve(&db.pool, owner)
            .await
            .expect("resolve")
            .expect("exists")
            .permissions
            .is_super_user()
    );
}

/// The owner's *stored* grants used to stop at whatever the codebase defined the day their
/// account was seeded. The seed is create-only, so `catalogue.read` and `catalogue.delete` —
/// added long after — were never written against the deployment owner, and every surface that
/// reads the rows rather than calling `PermissionSet::has` showed them as not holding the
/// capability. The access was fine; the console said otherwise, which is indistinguishable from
/// a broken grant when you are staring at an unticked box.
#[tokio::test]
async fn the_super_user_gains_a_stored_row_for_every_capability_added_after_their_account() {
    let db = TestDb::spawn().await;
    let owner = db
        .seed_user(
            "owner",
            &[Permission::SuperUser, Permission::UsersPermissions],
            AccountStatus::Active,
        )
        .await;
    let staff = db
        .seed_user("staff", &[Permission::UsersRead], AccountStatus::Active)
        .await;

    let added = permissions::grant_all_to_super_user(&db.pool)
        .await
        .expect("top up");
    assert!(
        added.contains(&Permission::CatalogueDelete.as_str().to_owned()),
        "a capability the owner lacked must be reported as newly granted"
    );
    assert!(
        !added.contains(&Permission::UsersPermissions.as_str().to_owned()),
        "a capability already held must not be reported as newly granted"
    );

    let held = permissions::resolve(&db.pool, owner)
        .await
        .expect("resolve")
        .expect("exists")
        .permissions;
    for permission in Permission::grantable() {
        assert!(
            held.iter().any(|p| p == permission),
            "{} must be stored against the owner, not merely implied",
            permission.as_str()
        );
    }
    assert!(
        held.is_super_user(),
        "the top-up must leave the grant that earned it in place"
    );

    // Nobody else is touched: the statement keys on the single super user row, so a deployment
    // with an administrator beside the owner does not quietly promote them too.
    assert_eq!(
        permissions::resolve(&db.pool, staff)
            .await
            .expect("resolve")
            .expect("exists")
            .permissions
            .len(),
        1
    );

    // Idempotent: every replica runs this at boot, and it runs again on the next one.
    assert!(
        permissions::grant_all_to_super_user(&db.pool)
            .await
            .expect("top up")
            .is_empty()
    );
}

/// A deployment with no owner must not have the top-up write grants to somebody. The statement
/// is driven by the super user row itself, so "no owner" has to mean "no rows written" rather
/// than falling back to a first-account guess.
#[tokio::test]
async fn the_top_up_writes_nothing_when_the_deployment_has_no_super_user() {
    let db = TestDb::spawn().await;
    let staff = db
        .seed_user(
            "staff",
            &[Permission::UsersPermissions],
            AccountStatus::Active,
        )
        .await;

    assert!(
        permissions::grant_all_to_super_user(&db.pool)
            .await
            .expect("top up")
            .is_empty()
    );
    assert_eq!(
        permissions::resolve(&db.pool, staff)
            .await
            .expect("resolve")
            .expect("exists")
            .permissions
            .len(),
        1
    );
}

/// The lockout guard asks "does anyone else hold this?" before every revoke, suspend and erase.
/// Counting the exact token alone would answer no while the super user was sitting there able
/// to grant it back, refusing an operation that was never dangerous.
#[tokio::test]
async fn a_super_user_counts_as_another_holder_of_every_capability() {
    let db = TestDb::spawn().await;
    let owner = db
        .seed_user("owner", &[Permission::SuperUser], AccountStatus::Active)
        .await;
    let admin = db
        .seed_user(
            "admin",
            &[Permission::UsersPermissions],
            AccountStatus::Active,
        )
        .await;

    let others = permissions::other_active_holders(&db.pool, Permission::UsersPermissions, admin)
        .await
        .expect("count holders");
    assert_eq!(others, 1, "the super user can grant the capability back");

    user_admin::set_status(&db.pool, owner, AccountStatus::Suspended, Some("test"))
        .await
        .expect("suspend the owner");
    let others = permissions::other_active_holders(&db.pool, Permission::UsersPermissions, admin)
        .await
        .expect("count holders");
    assert_eq!(others, 0, "a suspended super user cannot sign in to help");
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
