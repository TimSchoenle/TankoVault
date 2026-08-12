//! Chapter ingest against a real, migrated schema — pins the batching semantics
//! `chapter.discovered` depends on.
//!
//! Gated behind the `integration` feature (requires Docker).
#![cfg(feature = "integration")]

use tankovault_config::MatchingConfig;
use tankovault_db::repo::catalog::{
    ChapterUpsert, ScannedSeries, SeriesUpsert, ingest_series, upsert_chapters,
};
use tankovault_domain::{ContentType, MetadataPriority, SeriesStatus, normalize_title};
use tankovault_test_support::{TestDb, seed};

fn chapter(number: f64, title: Option<&str>, path: &str) -> ChapterUpsert {
    ChapterUpsert {
        number,
        volume: None,
        title: title.map(str::to_owned),
        path: path.to_owned(),
        published_at: None,
        access: tankovault_domain::ChapterAccess::Free,
        unlocks_at: None,
    }
}

fn scanned(
    provider_id: tankovault_domain::ProviderId,
    chapters: Vec<ChapterUpsert>,
) -> ScannedSeries {
    ScannedSeries {
        provider_id,
        source_path: "/manga/solo-leveling".to_owned(),
        provider_title: Some("Solo Leveling".to_owned()),
        meta: SeriesUpsert {
            canonical_title: "Solo Leveling".to_owned(),
            normalized_title: normalize_title("Solo Leveling"),
            description: None,
            cover_url: None,
            content_type: ContentType::Unknown,
            status: SeriesStatus::Unknown,
            release_year: None,
        },
        alt_titles: Vec::new(),
        tags: Vec::new(),
        authors: Vec::new(),
        chapters,
        content_hash: vec![1, 2, 3],
    }
}

/// A first ingest reports every chapter as new; re-ingesting the identical listing must report
/// none, or every watcher is re-notified on every scan cycle.
#[tokio::test]
async fn a_rescan_of_an_unchanged_listing_discovers_nothing() {
    let db = TestDb::spawn().await;
    let provider = seed::provider(&db, "ingest-rescan").create().await;

    // `ChapterUpsert` is deliberately not `Clone` — it is a one-shot ingest payload — so the
    // identical listing is rebuilt rather than cloned.
    let listing = || {
        vec![
            chapter(1.0, Some("Awakening"), "/c/1"),
            chapter(2.0, None, "/c/2"),
            chapter(3.5, Some("Interlude"), "/c/3-5"),
        ]
    };

    let first = ingest_series(
        &db.pool,
        &scanned(provider, listing()),
        &MatchingConfig::default(),
        &MetadataPriority::default(),
        &tankovault_domain::TagBlocklist::default(),
        &tankovault_domain::AdultTagSet::defaults(),
    )
    .await
    .expect("first ingest");
    assert_eq!(
        first.new_chapters,
        vec![1.0, 2.0, 3.5],
        "a first scan discovers every chapter, in ascending order"
    );

    let second = ingest_series(
        &db.pool,
        &scanned(provider, listing()),
        &MatchingConfig::default(),
        &MetadataPriority::default(),
        &tankovault_domain::TagBlocklist::default(),
        &tankovault_domain::AdultTagSet::defaults(),
    )
    .await
    .expect("second ingest");
    assert!(
        second.new_chapters.is_empty(),
        "a rescan of an unchanged listing must discover nothing, or every watcher is \
         re-notified on every cycle"
    );
    assert_eq!(second.series_id, first.series_id, "ingest is idempotent");
    assert_eq!(second.source_id, first.source_id);
}

/// Only the genuinely new chapter is reported when a listing grows, and an *edit* to an
/// existing chapter is applied without being announced as new.
#[tokio::test]
async fn only_added_chapters_are_reported_and_edits_are_applied_quietly() {
    let db = TestDb::spawn().await;
    let provider = seed::provider(&db, "ingest-growth").create().await;

    ingest_series(
        &db.pool,
        &scanned(provider, vec![chapter(1.0, Some("Old title"), "/c/1")]),
        &MatchingConfig::default(),
        &MetadataPriority::default(),
        &tankovault_domain::TagBlocklist::default(),
        &tankovault_domain::AdultTagSet::defaults(),
    )
    .await
    .expect("first ingest");

    let grown = ingest_series(
        &db.pool,
        &scanned(
            provider,
            vec![
                chapter(1.0, Some("Corrected title"), "/c/1-fixed"),
                chapter(2.0, None, "/c/2"),
            ],
        ),
        &MatchingConfig::default(),
        &MetadataPriority::default(),
        &tankovault_domain::TagBlocklist::default(),
        &tankovault_domain::AdultTagSet::defaults(),
    )
    .await
    .expect("second ingest");

    assert_eq!(
        grown.new_chapters,
        vec![2.0],
        "an edited chapter is updated, not re-announced"
    );

    let (title, path): (Option<String>, String) = sqlx::query_as(
        "SELECT c.title, c.path FROM chapters c \
           JOIN series_sources ss ON ss.id = c.series_source_id \
          WHERE ss.id = $1 AND c.number = 1",
    )
    .bind(grown.source_id.as_uuid())
    .fetch_one(&db.pool)
    .await
    .expect("the chapter is there");
    assert_eq!(title.as_deref(), Some("Corrected title"));
    assert_eq!(path, "/c/1-fixed");
}

/// A provider listing the same chapter number twice is real and recurring; `ON CONFLICT DO
/// UPDATE` aborts on a repeated row, so `DISTINCT ON` must keep the last spelling instead.
#[tokio::test]
async fn a_listing_that_repeats_a_chapter_number_does_not_abort_the_batch() {
    let db = TestDb::spawn().await;
    let provider = seed::provider(&db, "ingest-dupes").create().await;

    let outcome = ingest_series(
        &db.pool,
        &scanned(
            provider,
            vec![
                chapter(7.0, Some("First spelling"), "/c/7-a"),
                chapter(8.0, None, "/c/8"),
                chapter(7.0, Some("Last spelling"), "/c/7-b"),
            ],
        ),
        &MatchingConfig::default(),
        &MetadataPriority::default(),
        &tankovault_domain::TagBlocklist::default(),
        &tankovault_domain::AdultTagSet::defaults(),
    )
    .await
    .expect("a duplicated chapter number must not abort the ingest");

    assert_eq!(
        outcome.new_chapters,
        vec![7.0, 8.0],
        "the duplicate is one chapter, reported once"
    );

    let (title, count): (Option<String>, i64) = sqlx::query_as(
        "SELECT max(c.title), count(*) FROM chapters c \
          WHERE c.series_source_id = $1 AND c.number = 7",
    )
    .bind(outcome.source_id.as_uuid())
    .fetch_one(&db.pool)
    .await
    .expect("the chapter is there");
    assert_eq!(count, 1, "one row, not two");
    assert_eq!(
        title.as_deref(),
        Some("Last spelling"),
        "the last spelling wins, matching the row-at-a-time loop this replaced"
    );
}

/// Two chapter numbers that differ only past the fourth decimal are **one** row, because
/// `chapters.number` is `numeric(10,4)`.
///
/// The bug: `DISTINCT ON` deduplicated on the raw `float8`, so both rows survived it, and the
/// unique index then saw one key twice — "ON CONFLICT DO UPDATE command cannot affect row a
/// second time", which aborts the statement and fails the entire scan batch, not just the odd
/// chapter. The dedup key has to be the same cast expression the column stores.
#[tokio::test]
async fn chapter_numbers_that_round_to_one_value_do_not_abort_the_batch() {
    let db = TestDb::spawn().await;
    let provider = seed::provider(&db, "ingest-rounding").create().await;

    let outcome = ingest_series(
        &db.pool,
        &scanned(
            provider,
            vec![
                chapter(1.000_01, Some("First spelling"), "/c/1-a"),
                chapter(2.0, None, "/c/2"),
                chapter(1.000_02, Some("Last spelling"), "/c/1-b"),
            ],
        ),
        &MatchingConfig::default(),
        &MetadataPriority::default(),
        &tankovault_domain::TagBlocklist::default(),
        &tankovault_domain::AdultTagSet::defaults(),
    )
    .await
    .expect("numbers that collide only after rounding must not abort the ingest");

    let (title, count): (Option<String>, i64) = sqlx::query_as(
        "SELECT max(c.title), count(*) FROM chapters c \
          WHERE c.series_source_id = $1 AND c.number = 1",
    )
    .bind(outcome.source_id.as_uuid())
    .fetch_one(&db.pool)
    .await
    .expect("the rounded chapter is there");
    assert_eq!(count, 1, "1.00001 and 1.00002 are both chapter 1.0000");
    assert_eq!(
        title.as_deref(),
        Some("Last spelling"),
        "the last listing still wins, as it does for an exactly-repeated number"
    );
}

/// An empty listing must be a no-op rather than a malformed statement — a provider whose
/// chapter selector stopped matching produces exactly this.
#[tokio::test]
async fn an_empty_chapter_list_is_a_no_op() {
    let db = TestDb::spawn().await;
    let provider = seed::provider(&db, "ingest-empty").create().await;

    let outcome = ingest_series(
        &db.pool,
        &scanned(provider, Vec::new()),
        &MatchingConfig::default(),
        &MetadataPriority::default(),
        &tankovault_domain::TagBlocklist::default(),
        &tankovault_domain::AdultTagSet::defaults(),
    )
    .await
    .expect("an empty listing still ingests the series itself");
    assert!(outcome.new_chapters.is_empty());

    let direct = upsert_chapters(&db.pool, outcome.source_id, &[])
        .await
        .expect("the batch helper short-circuits");
    assert!(direct.is_empty());
}

/// A chapter whose paywall lifts must lose its unlock time in the same write.
///
/// `chapters_unlocks_at_requires_early_access` (migration `0047`) rejects a free row carrying an
/// `unlocks_at`, and the whole chapter batch is one statement — so a re-scan that freed a chapter
/// while leaving the stale date behind would not store a slightly-wrong row, it would abort the
/// ingest of every chapter of that series. The upsert overwrites both columns rather than
/// coalescing them, which is what makes the transition safe; this pins that, in both directions.
#[tokio::test]
async fn freeing_a_locked_chapter_clears_the_unlock_time_it_carried() {
    let db = TestDb::spawn().await;
    let provider = seed::provider(&db, "paywalled").create().await;
    let outcome = ingest_series(
        &db.pool,
        &scanned(provider, vec![chapter(1.0, None, "/c/1")]),
        &MatchingConfig::default(),
        &MetadataPriority::default(),
        &tankovault_domain::TagBlocklist::default(),
        &tankovault_domain::AdultTagSet::defaults(),
    )
    .await
    .expect("first ingest");

    let locked = ChapterUpsert {
        access: tankovault_domain::ChapterAccess::EarlyAccess,
        unlocks_at: Some(time::OffsetDateTime::now_utc() + time::Duration::days(7)),
        ..chapter(1.0, None, "/c/1")
    };
    upsert_chapters(&db.pool, outcome.source_id, &[locked])
        .await
        .expect("the chapter goes behind the paywall");
    let (access, unlocks): (
        tankovault_domain::ChapterAccess,
        Option<time::OffsetDateTime>,
    ) = sqlx::query_as(
        "SELECT access, unlocks_at FROM chapters WHERE series_source_id = $1 AND number = 1",
    )
    .bind(outcome.source_id.as_uuid())
    .fetch_one(&db.pool)
    .await
    .expect("read back");
    assert_eq!(access, tankovault_domain::ChapterAccess::EarlyAccess);
    assert!(unlocks.is_some(), "the stated unlock time is stored");

    // The timer expires and the next scan reports it free. Coalescing here would leave the date
    // behind and the CHECK constraint would take the whole batch down with it.
    upsert_chapters(&db.pool, outcome.source_id, &[chapter(1.0, None, "/c/1")])
        .await
        .expect("freeing the chapter must not violate the access CHECK");
    let (access, unlocks): (
        tankovault_domain::ChapterAccess,
        Option<time::OffsetDateTime>,
    ) = sqlx::query_as(
        "SELECT access, unlocks_at FROM chapters WHERE series_source_id = $1 AND number = 1",
    )
    .bind(outcome.source_id.as_uuid())
    .fetch_one(&db.pool)
    .await
    .expect("read back");
    assert_eq!(access, tankovault_domain::ChapterAccess::Free);
    assert!(
        unlocks.is_none(),
        "a free chapter cannot carry an unlock time"
    );
}

/// The series screen's chapter list is scoped to the reader asking for it.
///
/// Everything that screen shows is derived from this one list — the totals, the read/unread
/// split, and the "next up" marker — so a paywalled chapter left in it put a link to a paywall
/// under "next up" and made the counts disagree with every other surface. An anonymous visitor
/// sees what an anonymous visitor sees on the provider's own site: the free chapters.
#[tokio::test]
async fn the_chapter_list_hides_paid_chapters_from_readers_who_have_not_bought_them() {
    use tankovault_db::repo::catalog::list_chapters_across;

    let db = TestDb::spawn().await;
    let provider = seed::provider(&db, "paywalled").create().await;
    let reader = seed::user(&db, "reader").create().await;
    let subscriber = seed::user(&db, "subscriber").create().await;
    let outcome = ingest_series(
        &db.pool,
        &scanned(
            provider,
            vec![
                chapter(1.0, None, "/c/1"),
                chapter(2.0, None, "/c/2"),
                chapter(3.0, None, "/c/3"),
            ],
        ),
        &MatchingConfig::default(),
        &MetadataPriority::default(),
        &tankovault_domain::TagBlocklist::default(),
        &tankovault_domain::AdultTagSet::defaults(),
    )
    .await
    .expect("ingest");
    sqlx::query(
        "UPDATE chapters SET access = 'early_access', unlocks_at = now() + interval '7 days' \
         WHERE number = 3",
    )
    .execute(&db.pool)
    .await
    .expect("lock chapter 3");
    tankovault_db::repo::users::set_early_access_providers(&db.pool, subscriber, &[provider])
        .await
        .expect("opt in");

    let numbers = async |viewer| {
        list_chapters_across(&db.pool, &[outcome.source_id], viewer)
            .await
            .expect("list")
            .into_iter()
            .map(|c| c.number)
            .collect::<Vec<_>>()
    };

    assert_eq!(numbers(None).await, vec![2.0, 1.0], "anonymous");
    assert_eq!(numbers(Some(reader)).await, vec![2.0, 1.0], "not opted in");
    assert_eq!(
        numbers(Some(subscriber)).await,
        vec![3.0, 2.0, 1.0],
        "the reader who bought this provider's early access sees it"
    );

    // A stated unlock time that has passed frees the chapter for everyone, with no rescan — the
    // same rule the unread predicate applies, and the reason `unlocks_at` is stored at all.
    sqlx::query("UPDATE chapters SET unlocks_at = now() - interval '1 minute' WHERE number = 3")
        .execute(&db.pool)
        .await
        .expect("expire the timer");
    assert_eq!(numbers(None).await, vec![3.0, 2.0, 1.0]);
}
