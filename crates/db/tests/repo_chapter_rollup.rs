//! `chapter_rollup` against a real, migrated schema: the console's chapter figures must equal a
//! live count after every writer, and the verifier must repair what bypasses the triggers.
//!
//! Gated behind the `integration` feature (requires Docker).
#![cfg(feature = "integration")]

use tankovault_db::repo::catalog::maintenance::{delete_series, purge_chapters_batch, totals};
use tankovault_db::repo::catalog::rollup::verify_batch;
use tankovault_db::repo::catalog::{ChapterUpsert, upsert_chapters};
use tankovault_db::repo::matching::{merge_series, revert_merge};
use tankovault_db::repo::stats::{provider_stats, system_overview};
use tankovault_domain::{ChapterAccess, ProviderId, SeriesId, SeriesSourceId};
use tankovault_test_support::{TestDb, seed};
use time::OffsetDateTime;
use uuid::Uuid;

/// The header's chapter figures as they were counted before migration 0059, kept as the oracle.
const SYSTEM_BEFORE_0059: &str = "SELECT \
    (SELECT count(*) FROM chapters), \
    (SELECT count(*) FROM chapters WHERE discovered_at > now() - interval '1 hour'), \
    (SELECT count(*) FROM chapters WHERE discovered_at > now() - interval '24 hours'), \
    (SELECT count(*) FROM chapters WHERE discovered_at > now() - interval '7 days')";

/// The per-provider chapter figures as they were counted before migration 0059.
const PROVIDERS_BEFORE_0059: &str = "SELECT p.id, \
    COALESCE(ch.chapter_count, 0), COALESCE(ch.chapters_24h, 0), COALESCE(ch.chapters_7d, 0), \
    ch.last_chapter_at \
  FROM providers p LEFT JOIN ( \
    SELECT ss.provider_id, count(*) AS chapter_count, \
           count(*) FILTER (WHERE c.discovered_at > now() - interval '24 hours') AS chapters_24h, \
           count(*) FILTER (WHERE c.discovered_at > now() - interval '7 days') AS chapters_7d, \
           max(c.discovered_at) AS last_chapter_at \
    FROM series_sources ss JOIN chapters c ON c.series_source_id = ss.id \
    GROUP BY ss.provider_id) ch ON ch.provider_id = p.id \
  ORDER BY p.id";

type ProviderFigures = (Uuid, i64, i64, i64, Option<OffsetDateTime>);

/// Assert every chapter figure the console shows equals the pre-rollup live count.
async fn assert_figures_match_a_live_count(db: &TestDb, context: &str) {
    let live: (i64, i64, i64, i64) = sqlx::query_as(SYSTEM_BEFORE_0059)
        .fetch_one(&db.pool)
        .await
        .expect("live system figures");
    let header = system_overview(&db.pool).await.expect("system_overview");
    assert_eq!(
        (
            header.chapters_total,
            header.chapters_1h,
            header.chapters_24h,
            header.chapters_7d
        ),
        live,
        "header chapter figures after {context}"
    );

    let live: Vec<ProviderFigures> = sqlx::query_as(PROVIDERS_BEFORE_0059)
        .fetch_all(&db.pool)
        .await
        .expect("live provider figures");
    let mut stored: Vec<ProviderFigures> = provider_stats(&db.pool)
        .await
        .expect("provider_stats")
        .into_iter()
        .map(|p| {
            (
                p.provider_id,
                p.chapter_count,
                p.chapters_24h,
                p.chapters_7d,
                p.last_chapter_at,
            )
        })
        .collect();
    stored.sort_by_key(|p| p.0);
    assert_eq!(stored, live, "per-provider chapter figures after {context}");

    let total = live.iter().map(|p| p.1).sum::<i64>();
    assert_eq!(
        totals(&db.pool).await.expect("totals").chapters_total,
        total,
        "purge panel total after {context}"
    );
}

async fn first_source(db: &TestDb, series: SeriesId) -> (SeriesSourceId, String) {
    let (id, path): (Uuid, String) = sqlx::query_as(
        "SELECT id, source_path FROM series_sources WHERE series_id = $1 ORDER BY id LIMIT 1",
    )
    .bind(series.as_uuid())
    .fetch_one(&db.pool)
    .await
    .expect("source");
    (SeriesSourceId::from_uuid(id), path)
}

fn chapter(number: f64, title: &str) -> ChapterUpsert {
    ChapterUpsert {
        number,
        title: Some(title.to_owned()),
        path: format!("/c/{number}"),
        published_at: None,
        access: ChapterAccess::Free,
        unlocks_at: None,
    }
}

async fn a_series(db: &TestDb, provider: ProviderId, title: &str, chapters: &[f64]) -> SeriesId {
    seed::series(db, provider, title)
        .chapters(chapters)
        .create()
        .await
}

/// Insert chapters discovered at a fixed instant in the past, as a restore or a backfill would.
async fn insert_discovered_at(db: &TestDb, source: SeriesSourceId, numbers: &[i32], days_ago: i64) {
    sqlx::query(
        "INSERT INTO chapters (series_source_id, number_milli, path, discovered_at) \
         SELECT $1, n, '/old/' || n, now() - make_interval(days => $3::int) \
         FROM unnest($2::int[]) AS n",
    )
    .bind(source.as_uuid())
    .bind(numbers)
    .bind(days_ago)
    .execute(&db.pool)
    .await
    .expect("insert historic chapters");
}

/// Move one chapter's discovery instant, then move another chapter between sources, checking the
/// figures after each.
async fn move_chapters(db: &TestDb, from: SeriesSourceId, to: SeriesSourceId) {
    sqlx::query(
        "UPDATE chapters SET discovered_at = now() - interval '2 hours' \
         WHERE series_source_id = $1 AND number_milli = 40000",
    )
    .bind(from.as_uuid())
    .execute(&db.pool)
    .await
    .expect("move a discovery instant");
    assert_figures_match_a_live_count(db, "discovery instant moved").await;
    sqlx::query(
        "UPDATE chapters SET series_source_id = $2 \
         WHERE series_source_id = $1 AND number_milli = 50000",
    )
    .bind(from.as_uuid())
    .bind(to.as_uuid())
    .execute(&db.pool)
    .await
    .expect("move a chapter between sources");
    assert_figures_match_a_live_count(db, "chapter moved between sources").await;
}

/// Run `statements` on one connection with every trigger off, as a restore would.
async fn without_triggers(db: &TestDb, statements: &[&str]) {
    let mut conn = db.pool.acquire().await.expect("connection");
    sqlx::query("SET session_replication_role = replica")
        .execute(&mut *conn)
        .await
        .expect("triggers off");
    for statement in statements {
        // Test-authored literals and a formatted UUID, never input.
        sqlx::query(sqlx::AssertSqlSafe(*statement))
            .execute(&mut *conn)
            .await
            .expect(statement);
    }
    sqlx::query("SET session_replication_role = origin")
        .execute(&mut *conn)
        .await
        .expect("triggers on");
}

/// **Every writer of `chapters` keeps the console's chapter figures equal to a live count.**
///
/// The console used to count `chapters` whole on every refresh (7.8–48.7 s in production); it now
/// sums `chapter_rollup`, which is only as good as its triggers. Each step is a different writer,
/// through its repository function where one exists. The fold is the subtle part: a chapter
/// older than a week is counted in its source's folded row, and a delete has to find it there, or
/// the total drifts up forever with no error anywhere.
#[tokio::test]
async fn every_chapter_writer_keeps_the_console_figures_equal_to_a_live_count() {
    let db = TestDb::spawn().await;
    let alpha = seed::provider(&db, "alpha").create().await;
    let beta = seed::provider(&db, "beta").create().await;
    let keep = a_series(&db, alpha, "Vinland Saga", &[1.0, 2.0, 3.0]).await;
    let other = a_series(&db, beta, "Historie", &[2.0, 3.0, 4.0, 4.5]).await;
    let doomed = a_series(&db, beta, "Kingdom", &[1.0, 2.0]).await;
    assert_figures_match_a_live_count(&db, "seeding").await;

    let (source, path) = first_source(&db, keep).await;
    upsert_chapters(
        &db.pool,
        source,
        &path,
        &[chapter(4.0, "new"), chapter(5.0, "new")],
    )
    .await
    .expect("ingest");
    assert_figures_match_a_live_count(&db, "ingest").await;
    upsert_chapters(&db.pool, source, &path, &[chapter(4.0, "renamed")])
        .await
        .expect("rescan with a new title");
    assert_figures_match_a_live_count(&db, "title-only rescan").await;

    // History older than a week is folded on arrival.
    insert_discovered_at(&db, source, &[100_000, 110_000, 120_000], 30).await;
    assert_figures_match_a_live_count(&db, "historic insert").await;

    // Time passing, simulated with the triggers off: three recent chapters age past the week
    // while their row stays unfolded, as it does for a source nobody rescans.
    let (other_source, other_path) = first_source(&db, other).await;
    insert_discovered_at(&db, other_source, &[200_000, 210_000, 220_000], 2).await;
    without_triggers(
        &db,
        &[
            "UPDATE chapters SET discovered_at = discovered_at - interval '8 days' \
             WHERE number_milli IN (200000, 210000, 220000)",
            "UPDATE chapter_rollup SET discovered_at = discovered_at - interval '8 days', \
                    last_discovered_at = last_discovered_at - interval '8 days' \
             WHERE discovered_at > now() - interval '3 days' \
               AND discovered_at < now() - interval '1 day'",
        ],
    )
    .await;
    assert_figures_match_a_live_count(&db, "simulated ageing").await;
    sqlx::query("DELETE FROM chapters WHERE number_milli = 200000")
        .execute(&db.pool)
        .await
        .expect("delete an aged, unfolded chapter");
    assert_figures_match_a_live_count(&db, "deleting from an unfolded aged row").await;
    upsert_chapters(&db.pool, other_source, &other_path, &[chapter(9.0, "new")])
        .await
        .expect("ingest folds the aged row");
    let unfolded: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM chapter_rollup WHERE series_source_id = $1 \
         AND discovered_at > '-infinity' AND discovered_at < now() - interval '7 days'",
    )
    .bind(other_source.as_uuid())
    .fetch_one(&db.pool)
    .await
    .expect("unfolded rows");
    assert_eq!(unfolded, 0, "premise: the ingest folded the aged row");
    sqlx::query("DELETE FROM chapters WHERE number_milli = 210000")
        .execute(&db.pool)
        .await
        .expect("delete a folded chapter");
    assert_figures_match_a_live_count(&db, "deleting from the folded row").await;

    move_chapters(&db, source, other_source).await;

    // Merge and revert move sources between series, which changes no count.
    let undo = merge_series(&db.pool, keep, other, None, "merged")
        .await
        .expect("merge");
    assert_figures_match_a_live_count(&db, "merge").await;
    revert_merge(&db.pool, &undo).await.expect("revert");
    assert_figures_match_a_live_count(&db, "revert").await;

    // Deletion: the console's chapter purge, a series delete, a provider delete.
    let mut conn = db.pool.acquire().await.expect("connection");
    let (_, remaining) = purge_chapters_batch(&mut conn, 3).await.expect("purge");
    let live: i64 = sqlx::query_scalar("SELECT count(*) FROM chapters")
        .fetch_one(&db.pool)
        .await
        .expect("live count");
    assert_eq!(remaining, live, "the purge reports what is actually left");
    assert_figures_match_a_live_count(&db, "chapter purge batch").await;
    delete_series(&mut conn, &[doomed.as_uuid()])
        .await
        .expect("delete a series");
    drop(conn);
    assert_figures_match_a_live_count(&db, "series delete").await;
    tankovault_db::repo::providers::delete(&db.pool, beta)
        .await
        .expect("delete provider");
    assert_figures_match_a_live_count(&db, "provider delete").await;
}

/// **The verifier finds a source whose chapters were written around the triggers, and rebuilds it.**
///
/// A restore, or a session with `session_replication_role = replica`, writes chapters no trigger
/// sees; the console then shows a wrong total with no error anywhere. The verifier is what notices,
/// and its drift counts are what tell an operator it happened.
#[tokio::test]
async fn the_verifier_rebuilds_a_source_written_around_the_triggers() {
    let db = TestDb::spawn().await;
    let alpha = seed::provider(&db, "alpha").create().await;
    let first = a_series(&db, alpha, "Berserk", &[1.0, 2.0]).await;
    a_series(&db, alpha, "Vagabond", &[1.0]).await;

    let clean = verify_batch(&db.pool, None, 100).await.expect("verify");
    assert_eq!(
        (clean.checked, clean.drifted()),
        (2, 0),
        "a true table reports nothing"
    );
    assert_eq!(clean.resume_after, None, "a short batch reached the end");

    let (source, _) = first_source(&db, first).await;
    let bypass = format!(
        "INSERT INTO chapters (series_source_id, number_milli, path) VALUES ('{}', 90000, '/c/9')",
        source.as_uuid()
    );
    without_triggers(&db, &[&bypass]).await;

    // One source per batch: the cursor has to walk to the drifted one.
    let mut after = None;
    let mut found = tankovault_db::repo::catalog::rollup::RollupCheck::default();
    for _ in 0..2 {
        let check = verify_batch(&db.pool, after, 1).await.expect("verify");
        found.total += check.total;
        found.recent += check.recent;
        after = check.resume_after;
    }
    assert_eq!(
        (found.total, found.recent),
        (1, 1),
        "the bypassed write is both a total and a recent discovery"
    );
    assert_figures_match_a_live_count(&db, "rebuild").await;
    let stored: i64 = sqlx::query_scalar(
        "SELECT sum(chapters)::int8 FROM chapter_rollup WHERE series_source_id = $1",
    )
    .bind(source.as_uuid())
    .fetch_one(&db.pool)
    .await
    .expect("stored total");
    assert_eq!(stored, 3, "the rebuilt source counts the bypassed chapter");
    assert_eq!(
        verify_batch(&db.pool, None, 100)
            .await
            .expect("verify")
            .drifted(),
        0,
        "and the repair holds"
    );
}
