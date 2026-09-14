//! The pool's contract with a statement whose caller stopped waiting (`crates/db/src/pool.rs`).
//!
//! sqlx has no cancel request, so these properties live entirely in the pool settings and in
//! two server GUCs; nothing about them is visible to a type check.

#![cfg(feature = "integration")]

use std::time::{Duration, Instant};
use tankovault_db::{DbError, PoolSettings};
use tankovault_test_support::TestDb;

/// The text of the abandoned statement, so the activity probe can find exactly it.
const ABANDONED: &str = "SELECT pg_sleep(60)";

/// A dropped query neither holds its connection nor keeps running.
///
/// The bug this pins: a request hitting the API's 30 s timeout dropped its query future, and the
/// connection went back to the pool with the statement still executing. The next caller handed
/// that connection waited for the abandoned statement to finish before its own was sent, and
/// Postgres kept doing work nobody would read. The production slow log's 29.9 s / 0-row entries
/// were those drops, and each one queued the requests behind it.
#[tokio::test]
async fn a_dropped_statement_releases_its_connection_and_stops_running() {
    let db = TestDb::spawn().await;
    let pool = tankovault_db::connect_with(
        (*db.pool.connect_options()).clone(),
        PoolSettings::new(1, 30),
    )
    .await
    .expect("connect");

    let abandoned = tokio::time::timeout(
        Duration::from_millis(300),
        sqlx::query(ABANDONED).execute(&pool),
    )
    .await;
    assert!(
        abandoned.is_err(),
        "the sleep must still be running when it is dropped"
    );

    // One connection in the pool: this acquires the very connection the sleep was dropped on,
    // or its replacement.
    let started = Instant::now();
    sqlx::query("SELECT 1")
        .execute(&pool)
        .await
        .expect("next statement");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the next caller waited {:?} behind an abandoned statement",
        started.elapsed()
    );

    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let running: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_stat_activity \
             WHERE datname = current_database() AND state = 'active' AND query = $1",
        )
        .bind(ABANDONED)
        .fetch_one(&db.pool)
        .await
        .expect("read pg_stat_activity");
        if running == 0 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the abandoned statement was still executing 15 s after its caller left"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// A statement past the pool's ceiling fails as a cancellation the API can tell apart.
///
/// `ApiError` maps exactly this to `504` instead of a `500` filed as a Sentry issue, so the
/// SQLSTATE check has to keep matching what Postgres raises for `statement_timeout`.
#[tokio::test]
async fn a_statement_past_its_ceiling_is_cancelled() {
    let db = TestDb::spawn().await;
    let pool = tankovault_db::connect_with(
        (*db.pool.connect_options()).clone(),
        PoolSettings::new(1, 30).with_statement_timeout_secs(1),
    )
    .await
    .expect("connect");

    let error = sqlx::query("SELECT pg_sleep(10)")
        .execute(&pool)
        .await
        .expect_err("the ceiling must cancel the sleep");
    assert!(DbError::from(error).is_statement_cancelled());

    sqlx::query("SELECT 1")
        .execute(&pool)
        .await
        .expect("the connection survives its own cancellation");
}

/// A transaction-local ceiling outlasts the pool's, and does not outlive its transaction.
///
/// The admin console's rollups refresh behind a response under this override. Were the setting
/// session-wide instead, the connection would go back to the pool with a two-minute ceiling and
/// the next reader's statement on it would run uncapped.
#[tokio::test]
async fn a_raised_ceiling_applies_to_its_transaction_only() {
    let db = TestDb::spawn().await;
    let pool = tankovault_db::connect_with(
        (*db.pool.connect_options()).clone(),
        PoolSettings::new(1, 30).with_statement_timeout_secs(1),
    )
    .await
    .expect("connect");

    let mut tx = tankovault_db::begin_with_statement_timeout(&pool, Duration::from_secs(10))
        .await
        .expect("begin");
    sqlx::query("SELECT pg_sleep(1.5)")
        .execute(&mut *tx)
        .await
        .expect("the raised ceiling admits the sleep");
    tx.commit().await.expect("commit");

    let error = sqlx::query("SELECT pg_sleep(1.5)")
        .execute(&pool)
        .await
        .expect_err("the pool's own ceiling is back once the transaction ends");
    assert!(DbError::from(error).is_statement_cancelled());
}

/// A pool asked for custom plans keeps them past the fifth execution of a prepared statement.
///
/// The console's merge-decision journal, filtered to flagged decisions, returned nothing in
/// 1–30 s in production: after five executions the connection's prepared statement switched to a
/// generic plan, which cannot use the partial index its `NOT $6 OR flagged_at IS NOT NULL` arm
/// needs and walked the whole journal instead.
#[tokio::test]
async fn a_custom_plan_pool_never_switches_to_the_generic_plan() {
    let db = TestDb::spawn().await;
    let pool = tankovault_db::connect_with(
        (*db.pool.connect_options()).clone(),
        PoolSettings::new(1, 30).with_custom_plans(),
    )
    .await
    .expect("connect");

    for _ in 0..8 {
        sqlx::query("SELECT 1 WHERE $1::boolean IS NULL OR $1")
            .bind(true)
            .execute(&pool)
            .await
            .expect("prepared statement");
    }
    let mode: String = sqlx::query_scalar("SELECT current_setting('plan_cache_mode')")
        .fetch_one(&pool)
        .await
        .expect("read setting");
    assert_eq!(mode, "force_custom_plan");
}
