//! `series_browse` against a real, migrated schema: the projection must equal a recomputation
//! from the catalogue after every writer, under concurrent scans, and after the verifier runs.
//!
//! Gated behind the `integration` feature (requires Docker).
#![cfg(feature = "integration")]

use std::time::Duration;
use tankovault_db::repo::catalog::{update_source_scan, verify_projection};
use tankovault_db::repo::matching::{merge_series, revert_merge};
use tankovault_domain::{ProviderId, SeriesId, SeriesSourceId};
use tankovault_test_support::{TestDb, seed};
use uuid::Uuid;

/// Rows of `series_browse` and of its recomputation from the base tables that are not in both.
const PROJECTION_DRIFT: &str = "WITH live AS ( \
    SELECT s.id AS series_id, s.updated_at, s.content_type, s.status, s.release_year, \
           s.adult_gated, s.canonical_title, \
           COALESCE((SELECT max(chapter_count) FROM series_sources WHERE series_id = s.id), 0), \
           (SELECT count(DISTINCT provider_id)::int FROM series_sources WHERE series_id = s.id), \
           COALESCE((SELECT array_agg(DISTINCT provider_id ORDER BY provider_id) \
                     FROM series_sources WHERE series_id = s.id), '{}'), \
           COALESCE((SELECT array_agg(tag_id ORDER BY tag_id) \
                     FROM series_tags WHERE series_id = s.id), '{}') \
    FROM series s) \
  SELECT count(*) FROM ( \
    (TABLE live EXCEPT ALL SELECT * FROM series_browse) \
    UNION ALL \
    (SELECT * FROM series_browse EXCEPT ALL TABLE live)) d";

async fn assert_projection_is_current(db: &TestDb, context: &str) {
    let drift: i64 = sqlx::query_scalar(PROJECTION_DRIFT)
        .fetch_one(&db.pool)
        .await
        .expect("projection drift");
    assert_eq!(
        drift, 0,
        "series_browse rows disagree with the catalogue after {context}"
    );
}

async fn sources(db: &TestDb, series: SeriesId) -> Vec<SeriesSourceId> {
    sqlx::query_scalar::<_, Uuid>("SELECT id FROM series_sources WHERE series_id = $1 ORDER BY id")
        .bind(series.as_uuid())
        .fetch_all(&db.pool)
        .await
        .expect("sources")
        .into_iter()
        .map(SeriesSourceId::from_uuid)
        .collect()
}

async fn a_series(db: &TestDb, provider: ProviderId, title: &str, tags: &[&str]) -> SeriesId {
    seed::series(db, provider, title)
        .chapters(&[1.0, 2.0, 3.0])
        .tags(tags)
        .create()
        .await
}

/// **Every writer of a browse key keeps `series_browse` equal to the catalogue.**
///
/// Discover filters, sorts and counts on this projection alone, so a writer its triggers miss
/// makes a series vanish from a filter it matches, or sort by a chapter count it no longer has,
/// with no error anywhere. Each step is a different writer, through its repository function where
/// one exists.
#[tokio::test]
async fn the_browse_projection_follows_every_writer() {
    let db = TestDb::spawn().await;
    let alpha = seed::provider(&db, "alpha").create().await;
    let beta = seed::provider(&db, "beta").create().await;
    let keep = a_series(&db, alpha, "Vinland Saga", &["action", "historical"]).await;
    let other = a_series(&db, beta, "Historie", &["historical", "drama"]).await;
    let doomed = a_series(&db, beta, "Kingdom", &["war"]).await;
    assert_projection_is_current(&db, "ingest").await;

    // A scan that changes a chapter count, then one that changes nothing browse reads.
    let source = sources(&db, keep).await[0];
    update_source_scan(&db.pool, source, b"hash-2", 250)
        .await
        .expect("scan with a new count");
    assert_projection_is_current(&db, "scan with a changed chapter count").await;
    update_source_scan(&db.pool, source, b"hash-3", 250)
        .await
        .expect("scan with the same count");
    assert_projection_is_current(&db, "scan with an unchanged chapter count").await;

    // Series columns: an adult flag, and a metadata rewrite of every browse key at once.
    tankovault_db::repo::catalog::mark_adult_inferred(&db.pool, other)
        .await
        .expect("adult inferred");
    assert_projection_is_current(&db, "adult_inferred").await;
    sqlx::query(
        "UPDATE series SET canonical_title = 'Vinland Saga (Remastered)', status = 'completed', \
         content_type = 'manga', release_year = 2005, updated_at = now() - interval '3 days' \
         WHERE id = $1",
    )
    .bind(keep.as_uuid())
    .execute(&db.pool)
    .await
    .expect("rewrite series columns");
    assert_projection_is_current(&db, "series columns rewritten").await;

    // A merge moves sources and tags onto the survivor; its revert moves them back.
    let undo = merge_series(&db.pool, keep, other, None, "merged")
        .await
        .expect("merge");
    assert_projection_is_current(&db, "merge").await;
    revert_merge(&db.pool, &undo).await.expect("revert");
    assert_projection_is_current(&db, "revert").await;

    // Deletion: a vocabulary tag, a series, a provider.
    sqlx::query("DELETE FROM tags WHERE slug = 'historical'")
        .execute(&db.pool)
        .await
        .expect("delete a tag");
    assert_projection_is_current(&db, "vocabulary tag delete").await;
    let mut conn = db.pool.acquire().await.expect("connection");
    tankovault_db::repo::catalog::maintenance::delete_series(&mut conn, &[doomed.as_uuid()])
        .await
        .expect("delete a series");
    drop(conn);
    assert_projection_is_current(&db, "series delete").await;
    tankovault_db::repo::providers::delete(&db.pool, beta)
        .await
        .expect("delete provider");
    assert_projection_is_current(&db, "provider delete").await;
}

/// **Two scans of one series committing close together leave its chapter count current.**
///
/// The projection's refresh computes a series' `max_chapters` from its sources. Computed in the
/// same statement that writes it, the snapshot is taken before the statement waits for the row
/// lock: the second scan then stores a maximum that never saw the first scan's change. Here the
/// first scan raises one source from 100 to 500 and holds its transaction open; the second raises
/// the other to 200, computes 200 without the first's change, waits for the row, and overwrites
/// 500 with it.
#[tokio::test]
async fn concurrent_scans_of_one_series_do_not_store_a_stale_chapter_count() {
    let db = TestDb::spawn().await;
    let alpha = seed::provider(&db, "alpha").create().await;
    let beta = seed::provider(&db, "beta").create().await;
    let series = a_series(&db, alpha, "Berserk", &[]).await;
    sqlx::query(
        "INSERT INTO series_sources (series_id, provider_id, source_path) VALUES ($1, $2, '/b')",
    )
    .bind(series.as_uuid())
    .bind(beta.as_uuid())
    .execute(&db.pool)
    .await
    .expect("second source");
    let both = sources(&db, series).await;
    assert_eq!(both.len(), 2, "premise: one series on two sources");
    for source in &both {
        update_source_scan(&db.pool, *source, b"h", 100)
            .await
            .expect("baseline");
    }

    let mut first = db.pool.begin().await.expect("first scan");
    update_source_scan(&mut *first, both[0], b"h", 500)
        .await
        .expect("first scan raises");
    let pool = db.pool.clone();
    let second_source = both[1];
    let second = tokio::spawn(async move {
        update_source_scan(&pool, second_source, b"h", 200)
            .await
            .expect("second scan raises less");
    });
    // Long enough for the second scan to reach the row lock the first one holds.
    tokio::time::sleep(Duration::from_millis(500)).await;
    first.commit().await.expect("first scan commits");
    second.await.expect("second scan task");

    let stored: i32 =
        sqlx::query_scalar("SELECT max_chapters FROM series_browse WHERE series_id = $1")
            .bind(series.as_uuid())
            .fetch_one(&db.pool)
            .await
            .expect("stored count");
    assert_eq!(
        stored, 500,
        "the second scan stored a count computed before the first committed"
    );
    assert_projection_is_current(&db, "concurrent scans").await;
}

/// **The verifier restores a projection row a writer bypassed.**
///
/// A restore, or a write with the triggers off, leaves Discover filtering on keys that are no
/// longer true. The verifier is what notices, and its counts are what tell an operator.
#[tokio::test]
async fn the_verifier_repairs_stale_and_missing_projection_rows() {
    let db = TestDb::spawn().await;
    let alpha = seed::provider(&db, "alpha").create().await;
    let first = a_series(&db, alpha, "Berserk", &["action"]).await;
    let second = a_series(&db, alpha, "Vagabond", &["historical"]).await;

    let clean = verify_projection(&db.pool, None, 100)
        .await
        .expect("verify");
    assert_eq!(
        (clean.checked, clean.drifted()),
        (2, 0),
        "a true projection reports nothing"
    );
    assert_eq!(clean.resume_after, None, "a short batch reached the end");

    sqlx::query(
        "UPDATE series_browse SET max_chapters = 9999, tag_ids = '{}' WHERE series_id = $1",
    )
    .bind(first.as_uuid())
    .execute(&db.pool)
    .await
    .expect("corrupt a row");
    sqlx::query("DELETE FROM series_browse WHERE series_id = $1")
        .bind(second.as_uuid())
        .execute(&db.pool)
        .await
        .expect("lose a row");

    // One series per batch: the cursor has to walk to both.
    let mut after = None;
    let (mut missing, mut stale) = (0, 0);
    for _ in 0..2 {
        let check = verify_projection(&db.pool, after, 1).await.expect("verify");
        missing += check.missing;
        stale += check.stale;
        after = check.resume_after;
    }
    assert_eq!((missing, stale), (1, 1));
    assert_projection_is_current(&db, "verification").await;
    assert_eq!(
        verify_projection(&db.pool, None, 100)
            .await
            .expect("verify")
            .drifted(),
        0,
        "and the repair holds"
    );
}
