//! Watchlist backup and restore against a real schema: that an export identifies its series
//! without ids, that it restores into a database which has never seen those ids, and that an
//! entry whose series is not crawled yet waits rather than vanishing.
//!
//! Opt-in: gated behind the `integration` feature because it requires Docker.
#![cfg(feature = "integration")]

use tankovault_db::repo::tracking::{
    Commit, ImportOptions, progress_get_full, progress_set, watchlist_card, watchlist_export,
    watchlist_import, watchlist_list, watchlist_pending, watchlist_pending_sweep,
    watchlist_set_pinned_source, watchlist_upsert,
};
use tankovault_domain::watchlist_transfer::ConflictPolicy;
use tankovault_domain::{SeriesSourceId, WatchStatus};
use tankovault_test_support::{TestDb, seed};

const MERGE: ImportOptions = ImportOptions {
    policy: ConflictPolicy::KeepLocal,
    remove_missing: false,
    match_titles: false,
    include_adult: false,
};

/// The scenario the feature exists for: back up, wipe, restore before the catalogue is crawled
/// again, and have the entry attach once it is.
///
/// Uses a second database rather than deleting rows, so no uuid can survive between the two
/// halves; and names the provider differently on the restoring side, so the match has to come
/// from the source URL's host rather than from a slug that happens to agree.
#[tokio::test]
async fn a_backup_restores_into_a_fresh_database_once_its_series_is_crawled() {
    let old = TestDb::spawn().await;
    let provider = seed::provider(&old, "alpha")
        .base_url("https://alpha.example")
        .create()
        .await;
    let series = seed::series(&old, provider, "Berserk")
        .source_path("/series/berserk")
        .chapters(&[1.0, 2.0, 3.0])
        .create()
        .await;
    let reader = seed::user(&old, "reader").create().await;
    watchlist_upsert(&old.pool, reader, series, WatchStatus::Paused, false)
        .await
        .unwrap();
    progress_set(&old.pool, reader, series, 2.0).await.unwrap();

    let backup = watchlist_export(&old.pool, reader).await.unwrap();
    assert_eq!(backup.len(), 1);
    assert_eq!(
        backup[0].sources[0].url,
        "https://alpha.example/series/berserk"
    );

    let new = TestDb::spawn().await;
    let renamed = seed::provider(&new, "alpha-scans")
        .base_url("https://www.alpha.example/")
        .create()
        .await;
    let restorer = seed::user(&new, "reader").create().await;

    let outcome = watchlist_import(&new.pool, restorer, &backup, MERGE, Commit::Apply)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(outcome.plan.unmatched, vec![0]);
    assert!(
        watchlist_list(&new.pool, restorer)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        watchlist_pending(&new.pool, restorer, 10).await.unwrap().0,
        1
    );

    // Nothing to attach to yet: the sweep retries and leaves the entry queued.
    let idle = watchlist_pending_sweep(&new.pool, 100, false)
        .await
        .unwrap();
    assert_eq!((idle.examined, idle.attached), (1, 0));

    let crawled = seed::series(&new, renamed, "Berserk")
        .source_path("/series/berserk")
        .chapters(&[1.0, 2.0, 3.0])
        .create()
        .await;
    let swept = watchlist_pending_sweep(&new.pool, 100, false)
        .await
        .unwrap();
    assert_eq!(swept.attached, 1);

    let card = watchlist_card(&new.pool, restorer, crawled)
        .await
        .unwrap()
        .expect("the entry attached");
    assert_eq!(card.status, WatchStatus::Paused);
    assert!(!card.notify);
    let progress = progress_get_full(&new.pool, restorer, crawled)
        .await
        .unwrap()
        .unwrap();
    assert!((progress.last_read_whole_number - 2.0).abs() < f64::EPSILON);
    assert_eq!(
        watchlist_pending(&new.pool, restorer, 10).await.unwrap().0,
        0
    );
}

/// Merge keeps the reader's own choices and only moves progress forwards; replace makes the
/// watchlist the backup's.
#[tokio::test]
async fn merge_keeps_local_choices_and_replace_removes_what_the_backup_lacks() {
    let db = TestDb::spawn().await;
    let provider = seed::provider(&db, "alpha").create().await;
    let kept = seed::series(&db, provider, "Kept")
        .chapters(&[1.0])
        .create()
        .await;
    let extra = seed::series(&db, provider, "Extra")
        .chapters(&[1.0])
        .create()
        .await;
    let reader = seed::user(&db, "reader").create().await;

    watchlist_upsert(&db.pool, reader, kept, WatchStatus::Completed, true)
        .await
        .unwrap();
    progress_set(&db.pool, reader, kept, 5.0).await.unwrap();
    let mut backup = watchlist_export(&db.pool, reader).await.unwrap();
    backup[0].status = WatchStatus::Dropped;
    backup[0].progress.as_mut().unwrap().whole = 3.0;

    watchlist_upsert(&db.pool, reader, extra, WatchStatus::Reading, true)
        .await
        .unwrap();

    let merged = watchlist_import(&db.pool, reader, &backup, MERGE, Commit::Apply)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((merged.plan.unchanged, merged.removed), (1, 0));
    let card = watchlist_card(&db.pool, reader, kept)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(card.status, WatchStatus::Completed);
    let progress = progress_get_full(&db.pool, reader, kept)
        .await
        .unwrap()
        .unwrap();
    assert!((progress.last_read_whole_number - 5.0).abs() < f64::EPSILON);

    let replace = ImportOptions {
        policy: ConflictPolicy::PreferImported,
        remove_missing: true,
        ..MERGE
    };
    let replaced = watchlist_import(&db.pool, reader, &backup, replace, Commit::Apply)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(replaced.removed, 1);
    let entries = watchlist_list(&db.pool, reader).await.unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].status, WatchStatus::Dropped);
    let progress = progress_get_full(&db.pool, reader, kept)
        .await
        .unwrap()
        .unwrap();
    assert!((progress.last_read_whole_number - 3.0).abs() < f64::EPSILON);
}

/// A preview must be the real import rolled back: same counts, nothing written, nothing queued.
#[tokio::test]
async fn a_dry_run_writes_nothing() {
    let db = TestDb::spawn().await;
    let provider = seed::provider(&db, "alpha").create().await;
    let series = seed::series(&db, provider, "One")
        .chapters(&[1.0])
        .create()
        .await;
    let source_owner = seed::user(&db, "owner").create().await;
    watchlist_upsert(&db.pool, source_owner, series, WatchStatus::Reading, true)
        .await
        .unwrap();
    let mut backup = watchlist_export(&db.pool, source_owner).await.unwrap();
    let mut missing = backup[0].clone();
    missing.sources[0].path = "/nowhere".into();
    missing.sources[0].url = "https://alpha.invalid/nowhere".into();
    backup.push(missing);

    let reader = seed::user(&db, "reader").create().await;
    let outcome = watchlist_import(&db.pool, reader, &backup, MERGE, Commit::DryRun)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((outcome.plan.added, outcome.plan.unmatched.len()), (1, 1));
    assert!(watchlist_list(&db.pool, reader).await.unwrap().is_empty());
    assert_eq!(watchlist_pending(&db.pool, reader, 10).await.unwrap().0, 0);
}

/// An import must not be a way to learn which adult-gated series exist: for a reader who may not
/// see one, a backup naming it matches nothing.
#[tokio::test]
async fn an_adult_gated_series_is_not_matched_for_a_reader_who_cannot_see_it() {
    let db = TestDb::spawn().await;
    let provider = seed::provider(&db, "alpha").create().await;
    let gated = seed::series(&db, provider, "Gated")
        .chapters(&[1.0])
        .create()
        .await;
    let owner = seed::user(&db, "owner").create().await;
    watchlist_upsert(&db.pool, owner, gated, WatchStatus::Reading, true)
        .await
        .unwrap();
    let backup = watchlist_export(&db.pool, owner).await.unwrap();
    sqlx::query("UPDATE series SET is_adult = true WHERE id = $1")
        .bind(gated.as_uuid())
        .execute(&db.pool)
        .await
        .unwrap();

    let reader = seed::user(&db, "reader").create().await;
    let hidden = watchlist_import(&db.pool, reader, &backup, MERGE, Commit::DryRun)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(hidden.plan.unmatched, vec![0]);

    let visible = ImportOptions {
        include_adult: true,
        ..MERGE
    };
    let shown = watchlist_import(&db.pool, reader, &backup, visible, Commit::DryRun)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(shown.plan.added, 1);
}

/// A pinned source round-trips, and lands on the matching series' own source row.
#[tokio::test]
async fn a_pinned_source_survives_the_round_trip() {
    let db = TestDb::spawn().await;
    let alpha = seed::provider(&db, "alpha").create().await;
    let beta = seed::provider(&db, "beta").create().await;
    let series = seed::series(&db, alpha, "Pinned")
        .source_path("/p")
        .chapters(&[1.0])
        .create()
        .await;
    seed::series(&db, beta, "Pinned")
        .source_path("/p")
        .chapters(&[1.0])
        .create()
        .await;
    let owner = seed::user(&db, "owner").create().await;
    watchlist_upsert(&db.pool, owner, series, WatchStatus::Reading, true)
        .await
        .unwrap();
    let alpha_source: uuid::Uuid = sqlx::query_scalar(
        "SELECT id FROM series_sources WHERE series_id = $1 AND provider_id = $2",
    )
    .bind(series.as_uuid())
    .bind(alpha.as_uuid())
    .fetch_one(&db.pool)
    .await
    .unwrap();
    watchlist_set_pinned_source(
        &db.pool,
        owner,
        series,
        Some(SeriesSourceId::from_uuid(alpha_source)),
    )
    .await
    .unwrap();

    let backup = watchlist_export(&db.pool, owner).await.unwrap();
    assert!(
        backup[0]
            .sources
            .iter()
            .any(|s| s.pinned && s.provider == "alpha")
    );

    let reader = seed::user(&db, "reader").create().await;
    watchlist_import(&db.pool, reader, &backup, MERGE, Commit::Apply)
        .await
        .unwrap()
        .unwrap();
    let card = watchlist_card(&db.pool, reader, series)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        card.pinned_source_id,
        Some(SeriesSourceId::from_uuid(alpha_source))
    );
}
