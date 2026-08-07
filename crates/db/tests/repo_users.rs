//! Identity and session plumbing (`crates/db/src/repo/users.rs`, TEST F-05).
//!
//! Everything in this file decides whether somebody is let in, or stays let in. The queries
//! themselves are compile-checked, so what can go wrong is never a type error — it is a
//! *predicate*:
//!
//! - **Which column an identifier is routed to.** SEC-9's fix replaced `WHERE email = $1 OR
//!   username = $1` with two branches chosen on `@`. Losing a branch, or restoring the `OR`,
//!   compiles and passes every type check, and the failure is one account's password being
//!   checked against another account's row.
//! - **Whether a revocation is scoped to its owner.** `revoke_session` scopes through a
//!   subquery rather than the outer `WHERE`; dropping the `AND user_id = $1` inside it would
//!   let any signed-in account terminate any other account's session, and every existing
//!   caller would keep working.
//! - **Whether "single-use" is actually single-use.** `consume_*` is single-use only because of
//!   `AND used_at IS NULL`. Without it the update still reports a row, the handler still
//!   succeeds, and a reset link becomes replayable for its whole TTL.
//! - **Whether a changed email drops its verification** (SEC-4). The `CASE` that clears
//!   `email_verified_at` is three lines that nothing else in the codebase depends on, so it is
//!   exactly the shape of thing a later "simplification" removes.
//!
//! Every one of these fails *open*: the request succeeds, the response looks right, and the
//! only evidence is in somebody else's account. That is why they are pinned here rather than
//! left to the handler tests, which cannot see which row the database chose.
//!
//! Opt-in: gated behind the `integration` feature because it requires Docker.
#![cfg(feature = "integration")]

use tankovault_db::DbError;
use tankovault_db::repo::{user_admin, users};
use tankovault_domain::{AccountStatus, NotificationPrefs, StatusPrefs, UserId};
use tankovault_test_support::{TestDb, seed};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

/// The password hash is a placeholder: nothing in `crates/db` verifies one, and using a real
/// argon2 string here would only make the fixture slow.
fn a_hash(name: &str) -> String {
    format!("$argon2id$placeholder${name}")
}

/// Issue a refresh token and return the row id, which `insert_refresh` does not hand back.
async fn a_refresh(db: &TestDb, user: UserId, family: Uuid, token_hash: &str) -> Uuid {
    users::insert_refresh(
        &db.pool,
        user,
        family,
        token_hash,
        OffsetDateTime::now_utc() + Duration::days(30),
    )
    .await
    .expect("insert refresh token");
    users::find_refresh(&db.pool, token_hash)
        .await
        .expect("find refresh token")
        .expect("the token just inserted")
        .id
}

async fn revoked_at(db: &TestDb, token_hash: &str) -> Option<OffsetDateTime> {
    users::find_refresh(&db.pool, token_hash)
        .await
        .expect("find refresh token")
        .expect("token present")
        .revoked_at
}

/// Read `email_verified_at` directly: the repository only ever exposes it as a boolean, and
/// idempotence is a claim about the *instant*, not about the flag.
async fn email_verified_at(db: &TestDb, user: UserId) -> Option<OffsetDateTime> {
    sqlx::query_scalar("SELECT email_verified_at FROM users WHERE id = $1")
        .bind(user.as_uuid())
        .fetch_one(&db.pool)
        .await
        .expect("read email_verified_at")
}

async fn last_login_at(db: &TestDb, user: UserId) -> Option<OffsetDateTime> {
    sqlx::query_scalar("SELECT last_login_at FROM users WHERE id = $1")
        .bind(user.as_uuid())
        .fetch_one(&db.pool)
        .await
        .expect("read last_login_at")
}

async fn stored_email(db: &TestDb, user: UserId) -> String {
    sqlx::query_scalar("SELECT email::text FROM users WHERE id = $1")
        .bind(user.as_uuid())
        .fetch_one(&db.pool)
        .await
        .expect("read email")
}

// ---------------------------------------------------------------------------
// SEC-9 — the split credential lookup
// ---------------------------------------------------------------------------

/// An identifier containing `@` must resolve against `email` and nothing else.
///
/// The bug: `WHERE email = $1 OR username = $1` let user B register `victim@example.test` as a
/// *username* and collide with user A's *address*. Both rows matched, the planner picked one,
/// and whichever it picked decided whose `password_hash` the login handler verified — so A's
/// own correct password could authenticate as B, or B's password as A. The two unique
/// constraints are per-column, so nothing in the schema forbade the collision either.
///
/// The fixture drops the `username_not_an_email` CHECK (migration 0021) on purpose: 0021 stops
/// the state from being *created*, and this test asserts the lookup is unambiguous for data
/// that already exists — which is the property that made the routing worth doing on top of the
/// constraint rather than instead of it.
#[tokio::test]
async fn an_identifier_containing_an_at_sign_only_ever_resolves_as_an_email() {
    let db = TestDb::spawn().await;
    // Not `seed::user`: it writes one placeholder hash for every row, and the assertion below
    // is only worth making if the two rows carry *different* hashes — otherwise it holds just
    // as well when the lookup returns the squatter's credentials.
    let victim = users::create(&db.pool, "victim@example.test", "victim", &a_hash("victim"))
        .await
        .expect("seed the victim")
        .id;
    users::create(
        &db.pool,
        "squatter@example.test",
        "squatter",
        &a_hash("squatter"),
    )
    .await
    .expect("seed the squatter");

    db.execute("ALTER TABLE users DROP CONSTRAINT username_not_an_email")
        .await;
    db.execute("UPDATE users SET username = 'victim@example.test' WHERE username = 'squatter'")
        .await;

    let found = users::find_credentials(&db.pool, "victim@example.test")
        .await
        .expect("credential lookup")
        .expect("the address is registered");
    assert_eq!(
        found.user.id, victim,
        "an address must resolve to the account that owns it, never to an account that took \
         it as a username"
    );
    assert_eq!(found.password_hash, a_hash("victim"));
}

/// A bare identifier must resolve against `username` and nothing else.
///
/// The mirror of the case above, and the one that keeps the routing honest in both directions:
/// nothing in the schema requires `email` to look like an address, so an operator-created row
/// can hold a bare string there. If the lookup fell back to matching `email` as well, that row
/// would answer to a name its owner never chose as a username.
#[tokio::test]
async fn a_bare_identifier_only_ever_resolves_as_a_username() {
    let db = TestDb::spawn().await;
    seed::user(&db, "other").email("bare").create().await;

    assert!(
        users::find_credentials(&db.pool, "bare")
            .await
            .expect("credential lookup")
            .is_none(),
        "a bare identifier must not be matched against the email column"
    );
    assert!(
        users::find_credentials(&db.pool, "other")
            .await
            .expect("credential lookup")
            .is_some(),
    );
}

/// Both identifier columns are `citext`, so lookup and uniqueness agree on case.
///
/// **This test found a live defect.** `email` and `username` are `citext` (migrations 0001 and
/// 0004) so that `Alice@Example.com` and `alice@example.com` are one account — and the unique
/// index behaved that way — but every *lookup* was case-sensitive, because `sqlx` binds a Rust
/// `&str` as `text` and `citext = text` resolves, through citext's implicit cast to `text`, to
/// a plain `text = text`. Nothing in `find_credentials` was visibly wrong; the whole difference
/// was the OID on the wire, and the offline `.sqlx` cache recorded the parameter as `citext`,
/// so even reading the metadata said it was fine.
///
/// The user-visible failure was a silent, total lockout. Registration refused the second
/// casing as a duplicate, sign-in refused the first, and both password reset and
/// resend-confirmation answer identically whether or not an address is known (deliberately —
/// see `find_by_email`), so someone who capitalised their address differently than they
/// registered it could not sign in, could not recover, could not re-register, and was told
/// nothing. Fixed by binding through `repo::users::CiText`; see that type for why the fix is a
/// binding rather than a change to the SQL.
#[tokio::test]
async fn the_credential_lookup_is_case_insensitive_on_both_columns() {
    let db = TestDb::spawn().await;
    let user = seed::user(&db, "MixedCase")
        .email("mixed.case@example.test")
        .create()
        .await;

    for identifier in ["MIXED.CASE@EXAMPLE.TEST", "mixedcase", "MiXeDcAsE"] {
        let found = users::find_credentials(&db.pool, identifier)
            .await
            .expect("credential lookup")
            .unwrap_or_else(|| panic!("{identifier} must resolve"));
        assert_eq!(found.user.id, user, "identifier={identifier}");
    }
}

/// `email_verified` is derived, not stored: the lookup reports the flag the sign-in gate reads.
#[tokio::test]
async fn the_credential_lookup_reports_verification_and_status() {
    let db = TestDb::spawn().await;
    let user = seed::user(&db, "gate")
        .email("gate@example.test")
        .create()
        .await;

    let before = users::find_credentials(&db.pool, "gate")
        .await
        .expect("credential lookup")
        .expect("registered");
    assert!(
        !before.email_verified,
        "a freshly created account starts unverified"
    );
    assert_eq!(before.user.status, AccountStatus::Active);

    users::mark_email_verified(&db.pool, user)
        .await
        .expect("mark verified");
    user_admin::set_status(&db.pool, user, AccountStatus::Suspended, Some("test"))
        .await
        .expect("suspend");

    let after = users::find_credentials(&db.pool, "gate")
        .await
        .expect("credential lookup")
        .expect("registered");
    assert!(after.email_verified);
    assert_eq!(
        after.user.status,
        AccountStatus::Suspended,
        "suspension must reach the login path, which is the only place it is checked before a \
         session is issued"
    );
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Both identifiers are refused case-insensitively, and both report `Conflict`.
///
/// Case-insensitivity here is the schema's (`citext`), not the application's, so it is worth
/// pinning at this layer: a registration form that lowercases its input would hide a regression
/// in the column type until somebody registered through the admin console instead. The error
/// mapping matters too — a raw 23505 reaching the handler is a 500, and a registration form
/// that returns 500 for a taken name reads as an outage.
#[tokio::test]
async fn a_duplicate_email_or_username_is_refused_case_insensitively() {
    let db = TestDb::spawn().await;
    seed::user(&db, "taken")
        .email("taken@example.test")
        .create()
        .await;

    let cases = [
        ("taken@example.test", "fresh-a"),
        ("TAKEN@EXAMPLE.TEST", "fresh-b"),
        ("fresh-a@example.test", "taken"),
        ("fresh-b@example.test", "TAKEN"),
    ];
    for (email, username) in cases {
        let err = users::create(&db.pool, email, username, &a_hash(username))
            .await
            .expect_err(&format!("({email}, {username}) must be refused"));
        assert!(
            matches!(err, DbError::Conflict(_)),
            "({email}, {username}) produced {err:?}, not a Conflict"
        );
    }
}

/// The schema, not just the validator, refuses a username containing `@`.
///
/// SEC-9 was closed at three layers; this is the last one. A username reaching the database
/// through any path that skips `validate_username` — a seed script, the admin console, a future
/// import — must still be rejected, because it is the *stored* row that makes a lookup
/// ambiguous. Note this is a CHECK violation, not a uniqueness one, so it does **not** map to
/// `Conflict`: `create` only rewrites 23505.
#[tokio::test]
async fn the_schema_refuses_a_username_containing_an_at_sign() {
    let db = TestDb::spawn().await;

    let err = users::create(
        &db.pool,
        "impostor@example.test",
        "victim@example.test",
        &a_hash("impostor"),
    )
    .await
    .expect_err("a username containing @ must not be storable");
    assert!(
        matches!(err, DbError::Sqlx(_)),
        "expected the CHECK constraint to reject it, got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// Refresh-token rotation
// ---------------------------------------------------------------------------

/// Revoking one token must not touch its siblings, and must not touch another family.
///
/// `revoke_token` is the *normal rotation* path: it runs on every successful refresh, for a
/// token that is about to be replaced by its successor in the same family. If its `WHERE` ever
/// widened to the family (or to the user), every rotation would sign the user out of every
/// device — which looks like a session-expiry bug and gets "fixed" by lengthening the TTL.
#[tokio::test]
async fn revoking_one_token_leaves_its_siblings_and_other_families_alone() {
    let db = TestDb::spawn().await;
    let user = seed::user(&db, "rotate")
        .email("rotate@example.test")
        .create()
        .await;
    let laptop = Uuid::now_v7();
    let phone = Uuid::now_v7();

    let first = a_refresh(&db, user, laptop, "laptop-1").await;
    a_refresh(&db, user, laptop, "laptop-2").await;
    a_refresh(&db, user, phone, "phone-1").await;

    users::revoke_token(&db.pool, first)
        .await
        .expect("revoke token");

    assert!(revoked_at(&db, "laptop-1").await.is_some());
    assert!(
        revoked_at(&db, "laptop-2").await.is_none(),
        "rotating one token must not revoke its successor in the same family"
    );
    assert!(
        revoked_at(&db, "phone-1").await.is_none(),
        "rotating one token must not reach a different family"
    );
}

/// Revoking a family kills every live token in it, only that family, and only once.
///
/// This is the reuse-detection response: a presented token that is already revoked means the
/// lineage is compromised, so the whole family goes. Two properties, both load-bearing:
/// the blast radius is the family (not the account — a stolen laptop cookie must not sign the
/// phone out, or nobody will report the theft), and `AND revoked_at IS NULL` means a second
/// call cannot rewrite the instant the family was actually killed, which is the only record of
/// when the compromise was detected.
#[tokio::test]
async fn revoking_a_family_kills_that_family_once_and_no_other() {
    let db = TestDb::spawn().await;
    let user = seed::user(&db, "reuse")
        .email("reuse@example.test")
        .create()
        .await;
    let compromised = Uuid::now_v7();
    let untouched = Uuid::now_v7();

    a_refresh(&db, user, compromised, "bad-1").await;
    a_refresh(&db, user, compromised, "bad-2").await;
    a_refresh(&db, user, untouched, "good-1").await;

    users::revoke_family(&db.pool, compromised)
        .await
        .expect("revoke family");
    let first_kill = revoked_at(&db, "bad-1").await.expect("bad-1 revoked");
    assert!(revoked_at(&db, "bad-2").await.is_some());
    assert!(
        revoked_at(&db, "good-1").await.is_none(),
        "an unrelated family must survive reuse detection"
    );

    users::revoke_family(&db.pool, compromised)
        .await
        .expect("revoke family again");
    assert_eq!(
        revoked_at(&db, "bad-1").await,
        Some(first_kill),
        "re-revoking must not move the instant the family was killed"
    );
}

/// `find_refresh` returns revoked and expired tokens **on purpose**.
///
/// Filtering them would be the intuitive read of "find a valid token", and it would silently
/// disable reuse detection: a stolen token that has already been rotated would come back as
/// `None`, indistinguishable from a garbage cookie, so `session.rs` would answer 401 and leave
/// the family alive for the thief to keep rotating. The liveness filter belongs to
/// `list_sessions` — asserted here in the same test so the split of responsibility is visible
/// in one place — and to the handler's own `expires_at`/`revoked_at` checks.
#[tokio::test]
async fn find_refresh_returns_revoked_and_expired_tokens_for_reuse_detection() {
    let db = TestDb::spawn().await;
    let user = seed::user(&db, "detect")
        .email("detect@example.test")
        .create()
        .await;
    let family = Uuid::now_v7();

    let live = a_refresh(&db, user, family, "live").await;
    a_refresh(&db, user, family, "stale").await;
    a_refresh(&db, user, Uuid::now_v7(), "expired").await;

    users::revoke_token(&db.pool, live).await.expect("revoke");
    db.execute("UPDATE refresh_tokens SET expires_at = now() - interval '1 day' WHERE token_hash = 'expired'")
        .await;

    let revoked = users::find_refresh(&db.pool, "live")
        .await
        .expect("lookup")
        .expect("a revoked token is still findable");
    assert!(revoked.revoked_at.is_some());
    assert_eq!(revoked.user_id, user);
    assert_eq!(revoked.family_id, family);

    let expired = users::find_refresh(&db.pool, "expired")
        .await
        .expect("lookup")
        .expect("an expired token is still findable");
    assert!(
        expired.expires_at <= OffsetDateTime::now_utc(),
        "the caller can only refuse an expired token if it is handed the expiry"
    );

    let sessions = users::list_sessions(&db.pool, user)
        .await
        .expect("sessions");
    assert_eq!(
        sessions.iter().map(|s| s.id).collect::<Vec<_>>(),
        vec![
            users::find_refresh(&db.pool, "stale")
                .await
                .expect("lookup")
                .expect("present")
                .id
        ],
        "the session listing is where revoked and expired tokens are filtered out"
    );

    assert!(
        users::find_refresh(&db.pool, "never-issued")
            .await
            .expect("lookup")
            .is_none()
    );
}

// ---------------------------------------------------------------------------
// Password reset
// ---------------------------------------------------------------------------

/// A reset token is single-use, and the second attempt loses rather than both winning.
///
/// `consume_password_reset` is the whole guard: `AND used_at IS NULL` is what makes the flip
/// atomic, so of two concurrent resets presenting the same token exactly one sees `1`. Drop it
/// and both see `1` — the update still matches — so a reset link mailed once becomes replayable
/// for its entire TTL by anyone who reads the mailbox later.
#[tokio::test]
async fn a_password_reset_token_can_be_consumed_exactly_once() {
    let db = TestDb::spawn().await;
    let user = seed::user(&db, "reset")
        .email("reset@example.test")
        .create()
        .await;
    users::insert_password_reset(
        &db.pool,
        user,
        "reset-hash",
        OffsetDateTime::now_utc() + Duration::hours(1),
    )
    .await
    .expect("insert reset token");

    let record = users::find_password_reset(&db.pool, "reset-hash")
        .await
        .expect("lookup")
        .expect("present");
    assert_eq!(record.user_id, user);
    assert!(record.used_at.is_none());

    assert_eq!(
        users::consume_password_reset(&db.pool, record.id)
            .await
            .expect("consume"),
        1
    );
    assert_eq!(
        users::consume_password_reset(&db.pool, record.id)
            .await
            .expect("consume again"),
        0,
        "a consumed reset token must not be consumable a second time"
    );
    assert!(
        users::find_password_reset(&db.pool, "reset-hash")
            .await
            .expect("lookup")
            .expect("present")
            .used_at
            .is_some()
    );
}

/// Expiry is reported by the lookup and **not** enforced by `consume_password_reset`.
///
/// Worth stating explicitly because the split is easy to misread in either direction. The
/// repository deliberately answers "is this token known" separately from "is it usable", so the
/// handler can respond identically for unknown, expired and already-used tokens — which is what
/// stops the endpoint enumerating whether a reset was ever requested. The consequence is that
/// **every** caller must repeat the `expires_at` check (`services/api/src/auth/password.rs`
/// does); a new caller that only checks `consume`'s return value would honour an expired link.
#[tokio::test]
async fn an_expired_reset_token_is_reported_expired_but_the_repository_still_consumes_it() {
    let db = TestDb::spawn().await;
    let user = seed::user(&db, "stale-reset")
        .email("stale-reset@example.test")
        .create()
        .await;
    users::insert_password_reset(
        &db.pool,
        user,
        "stale-hash",
        OffsetDateTime::now_utc() + Duration::hours(1),
    )
    .await
    .expect("insert reset token");
    db.execute("UPDATE password_reset_tokens SET expires_at = now() - interval '1 second'")
        .await;

    let record = users::find_password_reset(&db.pool, "stale-hash")
        .await
        .expect("lookup")
        .expect("an expired token is still findable");
    assert!(
        record.expires_at <= OffsetDateTime::now_utc(),
        "the expiry the handler compares against must survive the round trip"
    );
    assert_eq!(
        users::consume_password_reset(&db.pool, record.id)
            .await
            .expect("consume"),
        1,
        "the expiry gate lives in the handler, not in this statement"
    );
}

/// A password reset replaces the hash and invalidates every live session for that user only.
///
/// `revoke_all_for_user` is the second half of a reset: the point is that a session stolen
/// before the reset dies with the credential it was minted from. Scoping is the risk — the
/// statement's only predicate on identity is `user_id = $1`.
#[tokio::test]
async fn a_password_reset_invalidates_only_the_resetting_users_sessions() {
    let db = TestDb::spawn().await;
    let subject = seed::user(&db, "subject")
        .email("subject@example.test")
        .create()
        .await;
    let bystander = seed::user(&db, "bystander")
        .email("bystander@example.test")
        .create()
        .await;
    a_refresh(&db, subject, Uuid::now_v7(), "subject-token").await;
    a_refresh(&db, bystander, Uuid::now_v7(), "bystander-token").await;

    users::update_password(&db.pool, subject, "$argon2id$rotated")
        .await
        .expect("update password");
    users::revoke_all_for_user(&db.pool, subject)
        .await
        .expect("revoke all");

    assert_eq!(
        users::find_credentials(&db.pool, "subject")
            .await
            .expect("lookup")
            .expect("present")
            .password_hash,
        "$argon2id$rotated"
    );
    assert!(revoked_at(&db, "subject-token").await.is_some());
    assert!(
        revoked_at(&db, "bystander-token").await.is_none(),
        "one user's password reset must not sign another user out"
    );
    assert!(
        matches!(
            users::update_password(&db.pool, UserId::new(), "$argon2id$nobody").await,
            Err(DbError::NotFound)
        ),
        "writing a password for a user that does not exist must fail, not silently no-op"
    );
}

// ---------------------------------------------------------------------------
// Email verification
// ---------------------------------------------------------------------------

/// Verification tokens are single-use, and confirming twice keeps the original instant.
///
/// `COALESCE(email_verified_at, now())` is what makes `mark_email_verified` idempotent. A plain
/// `SET email_verified_at = now()` would also pass every functional test — the flag is still
/// true — while quietly resetting "verified since" on every replay of the link, so the one
/// timestamp that could date an account takeover would always read as "just now".
#[tokio::test]
async fn email_verification_is_single_use_and_confirming_twice_keeps_the_first_instant() {
    let db = TestDb::spawn().await;
    let user = seed::user(&db, "confirm")
        .email("confirm@example.test")
        .create()
        .await;
    users::insert_email_verification(
        &db.pool,
        user,
        "verify-hash",
        OffsetDateTime::now_utc() + Duration::hours(24),
    )
    .await
    .expect("insert verification token");

    let record = users::find_email_verification(&db.pool, "verify-hash")
        .await
        .expect("lookup")
        .expect("present");
    assert_eq!(record.user_id, user);
    assert_eq!(
        users::consume_email_verification(&db.pool, record.id)
            .await
            .expect("consume"),
        1
    );
    assert_eq!(
        users::consume_email_verification(&db.pool, record.id)
            .await
            .expect("consume again"),
        0,
        "a consumed verification token must not be consumable a second time"
    );

    users::mark_email_verified(&db.pool, user)
        .await
        .expect("mark verified");
    let first = email_verified_at(&db, user).await.expect("verified");
    users::mark_email_verified(&db.pool, user)
        .await
        .expect("mark verified again");
    assert_eq!(
        email_verified_at(&db, user).await,
        Some(first),
        "re-confirming must leave the original verification instant untouched"
    );
}

/// The resend-confirmation lookup reports the flag, matches case-insensitively, and answers
/// `None` for an unregistered address.
///
/// `None` rather than an error is the anti-enumeration contract: the handler responds
/// identically either way, which it can only do if this call does not distinguish them by
/// failing. That silence is also what made the `citext` binding bug
/// ([`the_credential_lookup_is_case_insensitive_on_both_columns`]) so hard to see from the
/// outside — a registered address that failed to match looked exactly like an unregistered
/// one, and the user was told their mail was on its way either way.
#[tokio::test]
async fn the_resend_lookup_reports_verification_without_disclosing_registration() {
    let db = TestDb::spawn().await;
    let user = seed::user(&db, "resend")
        .email("resend@example.test")
        .create()
        .await;

    let (found, verified) = users::find_by_email_with_verification(&db.pool, "RESEND@EXAMPLE.TEST")
        .await
        .expect("lookup")
        .expect("registered");
    assert_eq!(found.id, user);
    assert!(!verified);

    users::mark_email_verified(&db.pool, user)
        .await
        .expect("mark verified");
    assert!(
        users::find_by_email_with_verification(&db.pool, "resend@example.test")
            .await
            .expect("lookup")
            .expect("registered")
            .1
    );

    assert!(
        users::find_by_email_with_verification(&db.pool, "nobody@example.test")
            .await
            .expect("lookup")
            .is_none()
    );
    assert!(
        users::find_by_email(&db.pool, "NOBODY@example.test")
            .await
            .expect("lookup")
            .is_none(),
        "the forgot-password entry point must answer None, not an error, for an unknown address"
    );
    assert_eq!(
        users::find_by_email(&db.pool, "RESEND@EXAMPLE.TEST")
            .await
            .expect("lookup")
            .expect("registered")
            .id,
        user
    );
}

// ---------------------------------------------------------------------------
// SEC-4 — a changed email loses its verification
// ---------------------------------------------------------------------------

/// Changing the address clears `email_verified_at`; changing only the username does not.
///
/// The bug SEC-4 closed: an attacker holding a 15-minute access token could point the account
/// at their own address, which arrived already "verified" because nothing reset the column,
/// then drive a password reset to it and lock the owner out of an account whose recovery
/// address they no longer controlled. The `CASE` that clears the column is the fix; the
/// username half is asserted alongside it because the cheap over-correction — clearing on every
/// `PATCH` — would force a re-verification email for a display-name edit, and would be reported
/// as a bug and reverted.
#[tokio::test]
async fn changing_the_email_clears_verification_and_changing_the_username_does_not() {
    let db = TestDb::spawn().await;
    let user = seed::user(&db, "owner")
        .email("owner@example.test")
        .create()
        .await;
    users::mark_email_verified(&db.pool, user)
        .await
        .expect("mark verified");

    let renamed = users::update_profile(&db.pool, user, Some("owner-renamed"), None)
        .await
        .expect("rename");
    assert_eq!(renamed.username, "owner-renamed");
    assert!(
        email_verified_at(&db, user).await.is_some(),
        "a username change must not force re-verification of an unchanged address"
    );

    let unchanged = users::update_profile(&db.pool, user, None, Some("owner@example.test"))
        .await
        .expect("no-op email write");
    assert_eq!(unchanged.email, "owner@example.test");
    assert!(
        email_verified_at(&db, user).await.is_some(),
        "writing the same address back must not force re-verification"
    );

    let moved = users::update_profile(&db.pool, user, None, Some("new-owner@example.test"))
        .await
        .expect("email change");
    assert_eq!(moved.email, "new-owner@example.test");
    assert!(
        email_verified_at(&db, user).await.is_none(),
        "a changed address must inherit nothing from the old one"
    );
}

/// A case-only email edit is written through, but is not treated as a change of address.
///
/// The third failing face of the `citext`-versus-`text` binding bug (see
/// [`the_credential_lookup_is_case_insensitive_on_both_columns`]). `update_profile` decides
/// whether the address *changed* with `$3 <> email`; bound as `text`, that comparison honoured
/// case, so correcting your own capitalisation counted as moving to a new address: SEC-4's
/// `CASE` cleared `email_verified_at`, the account was pushed back into the unverified state
/// that blocks sign-in, and a confirmation link went to the mailbox you were already using.
///
/// Both halves are pinned now: the new spelling is stored (the user sees the capitalisation
/// they typed) and the verification survives (two spellings deliver to the same mailbox, so
/// re-verification would be a papercut with no security value).
#[tokio::test]
async fn a_case_only_email_edit_is_stored_but_is_not_a_change_of_address() {
    let db = TestDb::spawn().await;
    let user = seed::user(&db, "casing")
        .email("casing@example.test")
        .create()
        .await;
    users::mark_email_verified(&db.pool, user)
        .await
        .expect("mark verified");

    users::update_profile(&db.pool, user, None, Some("Casing@Example.test"))
        .await
        .expect("case-only email edit");

    assert_eq!(stored_email(&db, user).await, "Casing@Example.test");
    assert!(
        email_verified_at(&db, user).await.is_some(),
        "two spellings of one address deliver to the same mailbox, so re-verification would be \
         a papercut with no security value"
    );
}

/// A profile update onto another account's identifier is a `Conflict`, not a 500.
///
/// `update_profile` repeats `create`'s 23505 rewrite; the two are separate `map_err` closures,
/// so the mapping can be present on one path and absent on the other. It is the *same* two
/// unique constraints either way.
#[tokio::test]
async fn a_profile_update_onto_a_taken_identifier_is_a_conflict() {
    let db = TestDb::spawn().await;
    seed::user(&db, "first")
        .email("first@example.test")
        .create()
        .await;
    let second = seed::user(&db, "second")
        .email("second@example.test")
        .create()
        .await;

    for (username, email) in [(None, Some("FIRST@example.test")), (Some("First"), None)] {
        let err = users::update_profile(&db.pool, second, username, email)
            .await
            .expect_err("must collide with the first account");
        assert!(
            matches!(err, DbError::Conflict(_)),
            "expected a Conflict, got {err:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

/// One user must not be able to revoke another user's session.
///
/// The ownership check is inside the subquery (`WHERE id = $2 AND user_id = $1`), not in the
/// outer `WHERE`, so it is invisible at the call site and easy to lose while "simplifying" the
/// statement to `family_id = (SELECT family_id FROM refresh_tokens WHERE id = $2)`. That
/// version compiles, passes every single-user test, and turns `DELETE /v1/me/sessions/{id}`
/// into a way for any signed-in account to sign out any other.
#[tokio::test]
async fn a_session_belonging_to_another_user_cannot_be_revoked() {
    let db = TestDb::spawn().await;
    let owner = seed::user(&db, "owner2")
        .email("owner2@example.test")
        .create()
        .await;
    let stranger = seed::user(&db, "stranger")
        .email("stranger@example.test")
        .create()
        .await;
    a_refresh(&db, owner, Uuid::now_v7(), "owner-token").await;

    let owner_session = users::list_sessions(&db.pool, owner)
        .await
        .expect("sessions")[0]
        .id;

    assert_eq!(
        users::revoke_session(&db.pool, stranger, owner_session)
            .await
            .expect("revoke"),
        0,
        "revoking a session that is not the caller's must affect no rows"
    );
    assert!(
        revoked_at(&db, "owner-token").await.is_none(),
        "the victim's session must still be live"
    );
    assert_eq!(
        users::revoke_session(&db.pool, owner, owner_session)
            .await
            .expect("revoke"),
        1,
        "the owner must still be able to revoke it"
    );
}

/// Revoking a session revokes its whole rotation family, and the session leaves the listing.
///
/// A "session" is a family, not a token: rotation mints a new row on every refresh, so revoking
/// only the row the user is looking at would sign them out for exactly one request cycle and no
/// longer. The listing is asserted in the same test because "did it work" is a question the
/// user asks by reloading the sessions page.
#[tokio::test]
async fn revoking_a_session_revokes_its_family_and_it_drops_out_of_the_listing() {
    let db = TestDb::spawn().await;
    let user = seed::user(&db, "multi")
        .email("multi@example.test")
        .create()
        .await;
    let laptop = Uuid::now_v7();
    let phone = Uuid::now_v7();

    let laptop_first = a_refresh(&db, user, laptop, "laptop-a").await;
    a_refresh(&db, user, laptop, "laptop-b").await;
    a_refresh(&db, user, phone, "phone-a").await;

    assert_eq!(
        users::list_sessions(&db.pool, user)
            .await
            .expect("sessions")
            .len(),
        3
    );
    assert_eq!(
        users::revoke_session(&db.pool, user, laptop_first)
            .await
            .expect("revoke"),
        2,
        "both live tokens in the rotation family must be revoked"
    );

    let remaining = users::list_sessions(&db.pool, user)
        .await
        .expect("sessions");
    assert_eq!(
        remaining.iter().map(|s| s.family_id).collect::<Vec<_>>(),
        vec![phone],
        "only the untouched family may remain listed"
    );
}

/// The listing is scoped to its user, ordered newest first, and excludes dead tokens.
#[tokio::test]
async fn the_session_listing_is_scoped_ordered_and_live_only() {
    let db = TestDb::spawn().await;
    let user = seed::user(&db, "listing")
        .email("listing@example.test")
        .create()
        .await;
    let other = seed::user(&db, "other-listing")
        .email("other-listing@example.test")
        .create()
        .await;

    let oldest = a_refresh(&db, user, Uuid::now_v7(), "oldest").await;
    a_refresh(&db, user, Uuid::now_v7(), "middle").await;
    a_refresh(&db, user, Uuid::now_v7(), "newest").await;
    a_refresh(&db, user, Uuid::now_v7(), "dead").await;
    a_refresh(&db, other, Uuid::now_v7(), "someone-else").await;

    // `created_at` defaults to `now()`, and four inserts inside one test can land on the same
    // transaction timestamp, so "newest first" is only well defined once it is pinned.
    db.execute(
        "UPDATE refresh_tokens SET created_at = timestamptz '2024-01-01 00:00:00Z' \
         + CASE token_hash \
             WHEN 'oldest' THEN interval '1 hour' \
             WHEN 'middle' THEN interval '2 hours' \
             ELSE interval '3 hours' END",
    )
    .await;
    db.execute(
        "UPDATE refresh_tokens SET expires_at = now() - interval '1 day' WHERE token_hash = 'dead'",
    )
    .await;
    users::revoke_token(&db.pool, oldest)
        .await
        .expect("revoke oldest");

    let listed = users::list_sessions(&db.pool, user)
        .await
        .expect("sessions");
    let mut expected = Vec::new();
    for hash in ["newest", "middle"] {
        expected.push(
            users::find_refresh(&db.pool, hash)
                .await
                .expect("lookup")
                .expect("present")
                .id,
        );
    }
    assert_eq!(
        listed.iter().map(|s| s.id).collect::<Vec<_>>(),
        expected,
        "the revoked and the expired token must both be excluded, and the survivors listed \
         newest first"
    );
    assert!(listed[0].expires_at > OffsetDateTime::now_utc());
    assert!(
        !users::list_sessions(&db.pool, other)
            .await
            .expect("sessions")
            .is_empty(),
        "the other user's session must be unaffected"
    );
}

/// A forced sign-out reports how many sessions it ended, and ends nobody else's.
///
/// The count is the whole reason `revoke_all_sessions` exists next to `revoke_all_for_user`: an
/// operator forcing a sign-out needs to be told whether there was anything to sign out of. The
/// second call returning `0` is what makes that number meaningful rather than a constant.
#[tokio::test]
async fn revoking_all_sessions_reports_the_count_and_is_scoped_to_one_user() {
    let db = TestDb::spawn().await;
    let user = seed::user(&db, "sweep")
        .email("sweep@example.test")
        .create()
        .await;
    let bystander = seed::user(&db, "sweep-other")
        .email("sweep-other@example.test")
        .create()
        .await;
    a_refresh(&db, user, Uuid::now_v7(), "sweep-1").await;
    a_refresh(&db, user, Uuid::now_v7(), "sweep-2").await;
    a_refresh(&db, bystander, Uuid::now_v7(), "sweep-other-1").await;

    assert_eq!(
        users::revoke_all_sessions(&db.pool, user)
            .await
            .expect("revoke all"),
        2
    );
    assert_eq!(
        users::revoke_all_sessions(&db.pool, user)
            .await
            .expect("revoke all again"),
        0,
        "a second forced sign-out has nothing left to revoke"
    );
    assert!(
        revoked_at(&db, "sweep-other-1").await.is_none(),
        "another account's session must survive"
    );
}

// ---------------------------------------------------------------------------
// Notification preferences, account state, last login
// ---------------------------------------------------------------------------

/// Preferences default to `{}` and round-trip unchanged.
///
/// An empty document means "use the product defaults", and the read is deliberately total: an
/// unknown id also answers the defaults rather than failing, because the column is `NOT NULL
/// DEFAULT '{}'` and the caller is always an authenticated principal. The write is *not* total —
/// it reports `NotFound` — and that asymmetry is pinned here so nobody reconciles it by making the
/// write silent, which would turn a settings save against a deleted account into a success.
///
/// The stored blob was free-form until the preferences became a typed contract; a document this
/// build cannot parse still has to read as the defaults, since failing here would cost the reader
/// the notification the preferences only ever meant to shape.
#[tokio::test]
async fn notification_prefs_default_and_round_trip() {
    let db = TestDb::spawn().await;
    let user = seed::user(&db, "prefs")
        .email("prefs@example.test")
        .create()
        .await;

    assert_eq!(
        users::get_notification_prefs(&db.pool, user)
            .await
            .expect("read prefs"),
        NotificationPrefs::default(),
        "an account that has never saved preferences reads as defaults"
    );

    let prefs = NotificationPrefs {
        watch_status: StatusPrefs {
            dropped: true,
            ..StatusPrefs::default()
        },
        ..NotificationPrefs::default()
    };
    users::set_notification_prefs(&db.pool, user, &prefs)
        .await
        .expect("write prefs");
    assert_eq!(
        users::get_notification_prefs(&db.pool, user)
            .await
            .expect("read prefs"),
        prefs
    );

    sqlx::query("UPDATE users SET notification_prefs = '[\"not a document\"]' WHERE id = $1")
        .bind(user.as_uuid())
        .execute(&db.pool)
        .await
        .expect("store an unparseable document");
    assert_eq!(
        users::get_notification_prefs(&db.pool, user)
            .await
            .expect("read prefs"),
        NotificationPrefs::default(),
        "an unparseable document reads as the defaults, not as an error"
    );

    let ghost = UserId::new();
    assert_eq!(
        users::get_notification_prefs(&db.pool, ghost)
            .await
            .expect("read prefs for an unknown id"),
        NotificationPrefs::default(),
        "the read is total"
    );
    assert!(
        matches!(
            users::set_notification_prefs(&db.pool, ghost, &prefs).await,
            Err(DbError::NotFound)
        ),
        "the write is not: saving settings for an account that does not exist must fail"
    );
}

/// `account_state` is the suspension check the authorization layer makes on every request.
///
/// `None` for an unknown id is the meaningful case: it is how a request bearing a still-valid
/// access token for a *deleted* account is refused. Returning a default `Active` state instead
/// would keep a deleted user signed in until their token expired.
#[tokio::test]
async fn account_state_reflects_suspension_and_is_absent_for_an_unknown_id() {
    let db = TestDb::spawn().await;
    let user = seed::user(&db, "state")
        .email("state@example.test")
        .create()
        .await;

    assert_eq!(
        users::account_state(&db.pool, user)
            .await
            .expect("read state")
            .expect("present")
            .status,
        AccountStatus::Active
    );

    user_admin::set_status(&db.pool, user, AccountStatus::Suspended, Some("test"))
        .await
        .expect("suspend");
    assert_eq!(
        users::account_state(&db.pool, user)
            .await
            .expect("read state")
            .expect("present")
            .status,
        AccountStatus::Suspended
    );

    assert!(
        users::account_state(&db.pool, UserId::new())
            .await
            .expect("read state")
            .is_none(),
        "an unknown id must be absent, not a default Active state"
    );

    // `get` answers the same question with the whole record, and reports the missing row as an
    // error rather than as `None` — the two are used at different layers and must not be
    // reconciled into one shape without deciding which caller changes.
    assert_eq!(
        users::get(&db.pool, user).await.expect("get").username,
        "state"
    );
    assert!(matches!(
        users::get(&db.pool, UserId::new()).await,
        Err(DbError::NotFound)
    ));
}

/// Only a completed sign-in advances `last_login_at` — looking the account up does not.
///
/// The timestamp is written by a separate statement precisely so a *failed* attempt cannot move
/// it. Folding it into `find_credentials` as an `UPDATE … RETURNING` would be one fewer round
/// trip and would make "last login" advance on every password guess, which is worse than not
/// recording it: both the operator reading the directory and the user checking their own
/// account would be told the attacker's guess was a successful sign-in.
#[tokio::test]
async fn only_a_completed_sign_in_advances_last_login() {
    let db = TestDb::spawn().await;
    let user = seed::user(&db, "login")
        .email("login@example.test")
        .create()
        .await;

    assert!(
        last_login_at(&db, user).await.is_none(),
        "a new account has never signed in"
    );
    users::find_credentials(&db.pool, "login@example.test")
        .await
        .expect("credential lookup");
    assert!(
        last_login_at(&db, user).await.is_none(),
        "looking credentials up is not signing in"
    );

    users::touch_last_login(&db.pool, user)
        .await
        .expect("touch last login");
    let first = last_login_at(&db, user).await.expect("recorded");

    users::touch_last_login(&db.pool, user)
        .await
        .expect("touch last login again");
    assert!(
        last_login_at(&db, user).await.expect("recorded") >= first,
        "a later sign-in must not move the timestamp backwards"
    );

    // Total, like the other write-by-id helpers that have no caller-visible failure: a sign-in
    // for an id that no longer exists is already impossible upstream.
    users::touch_last_login(&db.pool, UserId::new())
        .await
        .expect("touching an unknown id is a no-op, not an error");
}

// ---------------------------------------------------------------------------
// Source preferences
// ---------------------------------------------------------------------------

/// The order is the whole preference, so a write has to *replace* it.
///
/// Written as a differential against a naive upsert: an implementation that merged instead of
/// replacing would leave the demoted provider ranked and the positions non-contiguous, which the
/// unique index on `(user_id, position)` would then reject on the *next* write rather than this
/// one — a failure that surfaces one edit later, against a list the reader has moved on from.
#[tokio::test]
async fn saving_a_provider_order_replaces_the_previous_one() {
    let db = TestDb::spawn().await;
    let user = seed::user(&db, "ranker")
        .email("ranker@example.test")
        .create()
        .await;
    let first = seed::provider(&db, "alpha").create().await;
    let second = seed::provider(&db, "beta").create().await;

    users::set_provider_priority(&db.pool, user, &[first, second])
        .await
        .expect("save the order");
    let ranked = users::get_provider_priority(&db.pool, user)
        .await
        .expect("read the order");
    assert_eq!(
        ranked.iter().map(|p| p.slug.as_str()).collect::<Vec<_>>(),
        ["alpha", "beta"],
        "the stored order is the order given, not the insertion order"
    );

    users::set_provider_priority(&db.pool, user, &[second])
        .await
        .expect("replace the order");
    let ranked = users::get_provider_priority(&db.pool, user)
        .await
        .expect("read the replaced order");
    assert_eq!(
        ranked.iter().map(|p| p.slug.as_str()).collect::<Vec<_>>(),
        ["beta"],
        "a provider left out of the new order is unranked, not kept"
    );

    users::set_provider_priority(&db.pool, user, &[])
        .await
        .expect("clear the order");
    assert!(
        users::get_provider_priority(&db.pool, user)
            .await
            .expect("read the cleared order")
            .is_empty(),
        "an empty list clears the preference"
    );
}

/// A disabled provider carries nothing a reader can open, so it must not come back from a read —
/// otherwise the account panel offers a rank that can never apply and the Series screen ranks
/// against a provider that is not in any source list.
#[tokio::test]
async fn a_disabled_provider_drops_out_of_the_order() {
    let db = TestDb::spawn().await;
    let user = seed::user(&db, "disabled-rank")
        .email("disabled-rank@example.test")
        .create()
        .await;
    let provider = seed::provider(&db, "retired").create().await;
    users::set_provider_priority(&db.pool, user, &[provider])
        .await
        .expect("save the order");

    sqlx::query("UPDATE providers SET state = 'disabled' WHERE id = $1")
        .bind(provider.as_uuid())
        .execute(&db.pool)
        .await
        .expect("disable the provider");

    assert!(
        users::get_provider_priority(&db.pool, user)
            .await
            .expect("read the order")
            .is_empty(),
        "a disabled provider is not offered as a ranked source"
    );
}
