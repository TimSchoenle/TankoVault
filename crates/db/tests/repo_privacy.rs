//! GDPR portability (Art. 20) and erasure (Art. 17) against a real, migrated schema.
//!
//! # Why these are driven from `information_schema`
//!
//! The failure mode for both operations is **silent incompleteness**: a table added in a later
//! migration that stores something about a subject, and nobody remembering to add it to the
//! export or to check that erasure reaches it. Nothing fails, no error is logged, and the
//! deficiency only surfaces as a complaint to a regulator.
//!
//! So the two headline tests here do not hard-code a list of tables. They ask the live schema
//! which tables reference a user, and reconcile that answer against a declaration kept in this
//! file. A nineteenth migration that adds `user_notes(user_id …)` turns the build red on the
//! pull request that adds it, and the only ways to make it green are to export the table or to
//! write down, here, why it must not be.
//!
//! Opt-in: gated behind the `integration` feature because it requires Docker.
#![cfg(feature = "integration")]

use std::collections::BTreeSet;

use tankovault_db::repo::{privacy, users};
use tankovault_domain::{AccountStatus, Permission, UserId};
use tankovault_test_support::TestDb;

/// Every table whose rows are about a subject, paired with the key it appears under in the
/// export document.
///
/// This is the declaration the schema is reconciled against. Adding an entry here is the
/// *second* half of adding a table to the export; the first half is the SQL in
/// `repo::privacy::export_user_data`, and [`the_export_has_a_key_for_every_declared_table`]
/// checks that the two agree.
const EXPORTED: &[(&str, &str)] = &[
    ("users", "profile"),
    ("refresh_tokens", "sessions"),
    ("watchlist_entries", "watchlist"),
    ("read_progress", "read_progress"),
    ("notifications", "notifications"),
    ("external_accounts", "linked_accounts"),
    ("sync_remote_entries", "sync_remote_entries"),
    ("series_sync_overrides", "sync_overrides"),
    ("sync_conflicts", "sync_conflicts"),
    ("sync_history", "sync_history"),
    ("audit_log", "audit_entries"),
    ("user_permissions", "permissions"),
    ("gdpr_requests", "privacy_requests"),
];

/// Tables that reference a subject and are deliberately **not** exported, each with the reason.
///
/// An allow-list, not an oversight list: every entry here is a decision someone has to defend,
/// which is exactly the property that makes the reconciliation test worth having.
const NOT_EXPORTED: &[(&str, &str)] = &[
    (
        "password_reset_tokens",
        "live credential material — a valid reset token in an emailed export is an account \
         takeover, and the subject learns nothing from it",
    ),
    (
        "email_verification_tokens",
        "live credential material, same reasoning as password_reset_tokens",
    ),
    (
        "notification_dedup",
        "an idempotency guard, not a record about the person: every (series, chapter) in it is \
         already visible in `notifications`, which is exported",
    ),
];

/// The tables the live schema says hold a reference to a user.
///
/// Both the owning column names are looked for: `user_id` on the tables a subject owns, and
/// `actor_id`, which is how `audit_log` names the same relationship.
async fn tables_referencing_a_user(db: &TestDb) -> BTreeSet<String> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT table_name::text FROM information_schema.columns \
         WHERE table_schema = 'public' AND column_name IN ('user_id', 'actor_id')",
    )
    .fetch_all(&db.pool)
    .await
    .expect("read the live schema");

    let mut tables: BTreeSet<String> = rows.into_iter().collect();
    // `users` itself holds no `user_id`; it is the subject.
    tables.insert("users".to_owned());
    tables
}

async fn seed_subject(db: &TestDb, name: &str) -> UserId {
    db.seed_user(name, &[Permission::AuditRead], AccountStatus::Active)
        .await
}

#[tokio::test]
async fn every_table_that_references_a_subject_is_either_exported_or_documented_as_excluded() {
    // The guard against silent incompleteness. A migration that adds a table storing something
    // about a person fails here until it is either added to the export or written down in
    // NOT_EXPORTED with a reason — which is the review conversation this test exists to force.
    let db = TestDb::spawn().await;
    let in_schema = tables_referencing_a_user(&db).await;

    let accounted: BTreeSet<String> = EXPORTED
        .iter()
        .map(|(table, _)| (*table).to_owned())
        .chain(NOT_EXPORTED.iter().map(|(table, _)| (*table).to_owned()))
        .collect();

    let unaccounted: Vec<&String> = in_schema.difference(&accounted).collect();
    assert!(
        unaccounted.is_empty(),
        "these tables reference a subject but are neither exported nor listed as deliberately \
         excluded: {unaccounted:?}"
    );

    let stale: Vec<&String> = accounted.difference(&in_schema).collect();
    assert!(
        stale.is_empty(),
        "these tables are declared here but no longer exist in the schema: {stale:?}"
    );
}

#[tokio::test]
async fn the_export_has_a_key_for_every_declared_table() {
    // The other half of the reconciliation: the declaration above is only meaningful if the
    // SQL actually produces the keys it names. A table removed from `json_build_object`
    // without being removed from EXPORTED fails here.
    let db = TestDb::spawn().await;
    let subject = seed_subject(&db, "portability").await;

    let export = privacy::export_user_data(&db.pool, subject)
        .await
        .expect("export the subject");
    let document = export.as_object().expect("the export is a JSON object");

    for (table, key) in EXPORTED {
        assert!(
            document.contains_key(*key),
            "the export has no {key:?} key, so nothing from {table:?} can reach the subject"
        );
    }
    assert!(
        document.contains_key("exported_at"),
        "the export must say when it was taken"
    );
}

#[tokio::test]
async fn a_subject_with_no_activity_exports_empty_arrays_not_nulls() {
    // Stated by the doc comment on `export_user_data`, and load-bearing for whoever parses the
    // file: a `null` where an array was promised breaks a consumer on exactly the accounts
    // that are least interesting.
    let db = TestDb::spawn().await;
    let subject = seed_subject(&db, "quiet").await;

    let export = privacy::export_user_data(&db.pool, subject)
        .await
        .expect("export the subject");

    for (table, key) in EXPORTED {
        if *key == "profile" {
            continue; // a single object, not a collection
        }
        let value = &export[*key];
        assert!(
            value.is_array(),
            "{key:?} (from {table:?}) must be an array even when empty, got {value}"
        );
    }
    assert!(
        export["profile"].is_object(),
        "the profile must be present for an existing subject"
    );
}

#[tokio::test]
async fn the_export_carries_no_credential_material() {
    // The redaction contract, asserted on the *rendered document* rather than by reading the
    // SQL. A future `to_jsonb(u)` that forgot its `- 'password_hash'` would hand an offline
    // Argon2 cracking target to anyone who obtains a file people routinely forward by email.
    let db = TestDb::spawn().await;
    let subject = seed_subject(&db, "credentialed").await;

    // A live session, so `sessions` is non-empty and its redaction is actually exercised.
    users::insert_refresh(
        &db.pool,
        subject,
        uuid::Uuid::now_v7(),
        "a-recognisable-session-token-hash",
        time::OffsetDateTime::now_utc() + time::Duration::days(1),
    )
    .await
    .expect("seed a session");

    let export = privacy::export_user_data(&db.pool, subject)
        .await
        .expect("export the subject");
    assert!(
        !export["sessions"]
            .as_array()
            .expect("sessions is an array")
            .is_empty(),
        "the session fixture did not land, so this test would pass vacuously"
    );

    let rendered = export.to_string();
    for secret in [
        "password_hash",
        "token_hash",
        "a-recognisable-session-token-hash",
        "access_token",
        "refresh_token",
        "$argon2id$seed",
    ] {
        assert!(
            !rendered.contains(secret),
            "the export leaked {secret:?}: {rendered}"
        );
    }
}

#[tokio::test]
async fn the_export_names_no_third_party_from_the_subjects_own_audit_trail() {
    // Regression for SEC-15. The export used to dump whole `audit_log` rows. An operator's own
    // rows describe actions taken *on other people* — `admin/users.rs` records the edited
    // account's username and email in `detail` — so an operator's subject access request
    // disclosed other data subjects. Art. 15(4) forbids exactly that.
    let db = TestDb::spawn().await;
    let operator = seed_subject(&db, "operator").await;
    let bystander = db.seed_user("bystander", &[], AccountStatus::Active).await;

    tankovault_db::repo::audit::record(
        &db.pool,
        Some(operator),
        "admin.user.update",
        Some(&bystander.as_uuid().to_string()),
        &serde_json::json!({ "username": "bystander", "email": "bystander@example.test" }),
        "success",
        None,
        None,
    )
    .await
    .expect("record an administrative action taken on someone else");

    let export = privacy::export_user_data(&db.pool, operator)
        .await
        .expect("export the operator");
    let entries = export["audit_entries"]
        .as_array()
        .expect("audit_entries is an array");
    assert_eq!(
        entries.len(),
        1,
        "the operator's own action must still appear — the fix is a projection, not a removal"
    );
    assert_eq!(entries[0]["action"], "admin.user.update");

    let rendered = export.to_string();
    for third_party in [
        "bystander@example.test",
        &bystander.as_uuid().to_string(),
        "\"detail\"",
    ] {
        assert!(
            !rendered.contains(third_party),
            "the export named a third party ({third_party:?}): {rendered}"
        );
    }
}

#[tokio::test]
async fn erasing_a_subject_leaves_no_row_referencing_them_anywhere() {
    // Schema-driven for the same reason the export test is: a table added later with a
    // `user_id` that is *not* `ON DELETE CASCADE` would survive an erasure silently. This
    // walks every table the live schema says references a user and requires zero surviving
    // rows — except the two that are deliberately pseudonymised rather than deleted.
    let db = TestDb::spawn().await;
    let subject = seed_subject(&db, "erasable").await;

    users::insert_refresh(
        &db.pool,
        subject,
        uuid::Uuid::now_v7(),
        "session-to-be-erased",
        time::OffsetDateTime::now_utc() + time::Duration::days(1),
    )
    .await
    .expect("seed a session");
    users::insert_password_reset(
        &db.pool,
        subject,
        "reset-to-be-erased",
        time::OffsetDateTime::now_utc() + time::Duration::hours(1),
    )
    .await
    .expect("seed a reset token");
    tankovault_db::repo::gdpr::create(
        &db.pool,
        subject,
        tankovault_db::repo::gdpr::RequestKind::Erasure,
        Some("please erase me"),
    )
    .await
    .expect("seed a privacy request");
    tankovault_db::repo::audit::record(
        &db.pool,
        Some(subject),
        "auth.login",
        None,
        &serde_json::json!({}),
        "success",
        None,
        None,
    )
    .await
    .expect("seed an audit row");

    assert!(
        privacy::erase_user(&db.pool, subject)
            .await
            .expect("erase the subject"),
        "erasing an existing subject reports that it happened"
    );

    // `audit_log` and `gdpr_requests` are `ON DELETE SET NULL` by design: the accountability
    // record survives, the identity linking it to a person does not. Both are checked below
    // rather than skipped, because "the reference is gone" is the property that matters.
    let pseudonymised = ["audit_log", "gdpr_requests"];

    for table in tables_referencing_a_user(&db).await {
        let column = if table == "audit_log" {
            "actor_id"
        } else if table == "users" {
            "id"
        } else {
            "user_id"
        };
        let sql = format!("SELECT count(*) FROM {table} WHERE {column} = $1");
        let remaining: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
            .bind(subject.as_uuid())
            .fetch_one(&db.pool)
            .await
            .unwrap_or_else(|e| panic!("count surviving rows in {table}: {e}"));
        assert_eq!(
            remaining, 0,
            "{table} still holds {remaining} row(s) referencing the erased subject"
        );
    }

    for table in pseudonymised {
        let sql = format!("SELECT count(*) FROM {table}");
        let total: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
            .fetch_one(&db.pool)
            .await
            .unwrap_or_else(|e| panic!("count rows in {table}: {e}"));
        assert!(
            total > 0,
            "{table} was deleted rather than pseudonymised; the accountability record is gone"
        );
    }
}

#[tokio::test]
async fn erasing_a_subject_that_is_already_gone_reports_that_it_was_not_there() {
    // The caller distinguishes "erased" from "was already gone" off this boolean, without a
    // prior existence check that would race with a concurrent erasure.
    let db = TestDb::spawn().await;
    let subject = seed_subject(&db, "twiceerased").await;

    assert!(privacy::erase_user(&db.pool, subject).await.expect("erase"));
    assert!(
        !privacy::erase_user(&db.pool, subject)
            .await
            .expect("erase again"),
        "a second erasure must report that there was nothing to erase"
    );
}
