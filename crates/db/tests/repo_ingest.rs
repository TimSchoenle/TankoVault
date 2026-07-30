//! Chapter ingest against a real, migrated schema.
//!
//! `ingest_series` used to upsert chapters one row at a time inside a single transaction that
//! also holds row locks on the shared `tags` and `authors` rows — so a series with two
//! thousand chapters meant two thousand sequential round trips, and every other provider's
//! ingest queued behind it. Batching it into one statement is a meaningful change to a path
//! whose output drives `chapter.discovered` notifications, so the semantics it has to preserve
//! are pinned here rather than assumed.
//!
//! Opt-in: gated behind the `integration` feature because it requires Docker.
#![cfg(feature = "integration")]

use tankovault_config::MatchingConfig;
use tankovault_db::repo::catalog::{
    ChapterUpsert, ScannedSeries, SeriesUpsert, ingest_series, upsert_chapters,
};
use tankovault_db::repo::providers::{self, NewProvider};
use tankovault_domain::{AdapterKind, ContentType, Politeness, SeriesStatus, normalize_title};
use tankovault_test_support::TestDb;

fn chapter(number: f64, title: Option<&str>, path: &str) -> ChapterUpsert {
    ChapterUpsert {
        number,
        volume: None,
        title: title.map(str::to_owned),
        path: path.to_owned(),
        published_at: None,
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

async fn a_provider(db: &TestDb, slug: &str) -> tankovault_domain::ProviderId {
    providers::create(
        &db.pool,
        NewProvider {
            slug: slug.to_owned(),
            name: slug.to_owned(),
            base_url: "https://example.test".to_owned(),
            adapter: AdapterKind::Madara,
            config: serde_json::json!({}),
            politeness: Politeness::default(),
        },
    )
    .await
    .expect("provider inserts")
    .id
}

/// The contract `chapter.discovered` rests on: a first ingest reports every chapter as new,
/// and re-ingesting the identical listing reports none. Getting the second half wrong would
/// re-notify every watcher on every scan cycle.
#[tokio::test]
async fn a_rescan_of_an_unchanged_listing_discovers_nothing() {
    let db = TestDb::spawn().await;
    let provider = a_provider(&db, "ingest-rescan").await;

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
    let provider = a_provider(&db, "ingest-growth").await;

    ingest_series(
        &db.pool,
        &scanned(provider, vec![chapter(1.0, Some("Old title"), "/c/1")]),
        &MatchingConfig::default(),
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

/// The reason the batch uses `DISTINCT ON`.
///
/// `ON CONFLICT DO UPDATE` cannot touch the same row twice within one statement — Postgres
/// raises SQLSTATE 21000, "ON CONFLICT DO UPDATE command cannot affect row a second time".
/// A provider listing the same chapter number twice on one page is real and recurring, and
/// the row-at-a-time loop this replaced simply applied the last one. So must the batch, and
/// it must not error.
#[tokio::test]
async fn a_listing_that_repeats_a_chapter_number_does_not_abort_the_batch() {
    let db = TestDb::spawn().await;
    let provider = a_provider(&db, "ingest-dupes").await;

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

/// An empty listing must be a no-op rather than a malformed statement — a provider whose
/// chapter selector stopped matching produces exactly this.
#[tokio::test]
async fn an_empty_chapter_list_is_a_no_op() {
    let db = TestDb::spawn().await;
    let provider = a_provider(&db, "ingest-empty").await;

    let outcome = ingest_series(
        &db.pool,
        &scanned(provider, Vec::new()),
        &MatchingConfig::default(),
    )
    .await
    .expect("an empty listing still ingests the series itself");
    assert!(outcome.new_chapters.is_empty());

    let direct = upsert_chapters(&db.pool, outcome.source_id, &[])
        .await
        .expect("the batch helper short-circuits");
    assert!(direct.is_empty());
}
