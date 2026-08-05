//! The reader's derived recommendation state (`crates/db/src/repo/recsys/reader.rs`).
//!
//! Gated behind the `integration` feature (requires Docker).
#![cfg(feature = "integration")]

use tankovault_db::repo::recsys;
use tankovault_domain::{ProviderId, SeriesId, UserId};
use tankovault_test_support::{TestDb, seed};
use time::OffsetDateTime;

/// The affinity a profile rebuild would write for `series`, all of it plausible and in range.
fn rows(series: &[SeriesId], engagement: f32) -> (Vec<f32>, Vec<f32>, Vec<OffsetDateTime>) {
    let now = OffsetDateTime::now_utc();
    (
        series.iter().map(|_| 0.5).collect(),
        series.iter().map(|_| engagement).collect(),
        series.iter().map(|_| now).collect(),
    )
}

async fn a_series(db: &TestDb, provider: ProviderId, title: &str) -> SeriesId {
    seed::series(db, provider, title).create().await
}

async fn replace(db: &TestDb, user: UserId, series: &[SeriesId]) {
    let (affinities, engagements, observed) = rows(series, 1.0);
    recsys::replace_affinity(&db.pool, user, series, &affinities, &engagements, &observed)
        .await
        .expect("replace affinity");
}

async fn affinity_series(db: &TestDb, user: UserId) -> Vec<SeriesId> {
    let mut ids: Vec<SeriesId> = recsys::top_affinity(&db.pool, user, 100)
        .await
        .expect("read affinity")
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    ids.sort_by_key(|id| id.as_uuid());
    ids
}

/// **A rebuild that fails leaves the affinity the reader already had.**
///
/// The bug: `replace_affinity` was a `DELETE` followed by an `INSERT`, both issued on the pool
/// and so each committing on its own. When the insert failed — which it did on every request, for
/// any reader with a series past the engagement knee, because `engagement` was computed unclamped
/// against a `CHECK (engagement <= 1)` column — the delete had already gone through. So the 500
/// the reader saw came with their whole derived profile deleted, and the next rebuild started
/// from nothing.
///
/// An out-of-range engagement is the trigger here because it is the one production hit, and it
/// stays a valid trigger for any other statement-level failure.
#[tokio::test]
async fn a_failed_rebuild_leaves_the_previous_affinity_intact() {
    let db = TestDb::spawn().await;
    let provider = seed::provider(&db, "recsys-atomic").create().await;
    let user = seed::user(&db, "reader").create().await;
    let series = a_series(&db, provider, "Tracked").await;

    replace(&db, user, &[series]).await;

    let ids = [series];
    let (affinities, engagements, observed) = rows(&ids, 1.5);
    let failed =
        recsys::replace_affinity(&db.pool, user, &ids, &affinities, &engagements, &observed).await;
    assert!(failed.is_err(), "an engagement of 1.5 must be rejected");

    assert_eq!(
        affinity_series(&db, user).await,
        vec![series],
        "the failed rebuild rolled back onto nothing"
    );
}

/// A repeat rebuild updates the rows in place instead of colliding with them.
///
/// The reason it is an upsert rather than a fresh insert is concurrency — the SPA rebuilds one
/// reader's stale profile from several surfaces at once — which no sequential test can force. So
/// this pins the half that is deterministic: the `ON CONFLICT` target has to match the primary
/// key, or the second rebuild is a duplicate-key error.
#[tokio::test]
async fn rebuilding_affinity_twice_updates_rather_than_conflicts() {
    let db = TestDb::spawn().await;
    let provider = seed::provider(&db, "recsys-idempotent").create().await;
    let user = seed::user(&db, "reader").create().await;
    let first = a_series(&db, provider, "First").await;
    let second = a_series(&db, provider, "Second").await;

    replace(&db, user, &[first, second]).await;
    replace(&db, user, &[first, second]).await;

    let mut expected = vec![first, second];
    expected.sort_by_key(|id| id.as_uuid());
    assert_eq!(affinity_series(&db, user).await, expected);
}

/// A series the reader stopped tracking loses its affinity row, or it keeps seeding
/// recommendations for something they said they were done with.
///
/// The empty case is not redundant: pruning is now a predicate rather than an unconditional
/// delete, and `series_id <> ALL('{}')` is the one input where reading it wrong is silent.
#[tokio::test]
async fn a_series_dropped_from_the_rebuild_loses_its_row() {
    let db = TestDb::spawn().await;
    let provider = seed::provider(&db, "recsys-prune").create().await;
    let user = seed::user(&db, "reader").create().await;
    let kept = a_series(&db, provider, "Kept").await;
    let removed = a_series(&db, provider, "Removed").await;

    replace(&db, user, &[kept, removed]).await;
    replace(&db, user, &[kept]).await;
    assert_eq!(affinity_series(&db, user).await, vec![kept]);

    replace(&db, user, &[]).await;
    assert!(affinity_series(&db, user).await.is_empty());
}
