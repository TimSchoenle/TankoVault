//! Chapter ingest against a real, migrated schema, pinning the batching semantics
//! `chapter.discovered` depends on.
//!
//! Gated behind the `integration` feature (requires Docker).
#![cfg(feature = "integration")]

use tankovault_config::MatchingConfig;
use tankovault_db::repo::catalog::{
    ChapterUpsert, ScannedSeries, SeriesUpsert, ingest_series, register_source_stub,
    upsert_chapters,
};
use tankovault_domain::{ContentType, MetadataPriority, SeriesStatus, normalize_title};
use tankovault_test_support::{TestDb, seed};

fn chapter(number: f64, title: Option<&str>, path: &str) -> ChapterUpsert {
    ChapterUpsert {
        number,
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

    // `ChapterUpsert` is a one-shot ingest payload and deliberately not `Clone`, so the identical
    // listing is rebuilt.
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
        &tankovault_domain::TermBlocklist::default(),
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
        &tankovault_domain::TermBlocklist::default(),
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
        &tankovault_domain::TermBlocklist::default(),
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
        &tankovault_domain::TermBlocklist::default(),
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
          WHERE ss.id = $1 AND c.number_milli = 10000",
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
        &tankovault_domain::TermBlocklist::default(),
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
          WHERE c.series_source_id = $1 AND c.number_milli = 70000",
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
/// unique index then saw one key twice: "ON CONFLICT DO UPDATE command cannot affect row a
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
        &tankovault_domain::TermBlocklist::default(),
        &tankovault_domain::AdultTagSet::defaults(),
    )
    .await
    .expect("numbers that collide only after rounding must not abort the ingest");

    let (title, count): (Option<String>, i64) = sqlx::query_as(
        "SELECT max(c.title), count(*) FROM chapters c \
          WHERE c.series_source_id = $1 AND c.number_milli = 10000",
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

/// An empty listing must be a no-op, not a malformed statement: a provider whose chapter
/// selector stopped matching produces exactly this.
#[tokio::test]
async fn an_empty_chapter_list_is_a_no_op() {
    let db = TestDb::spawn().await;
    let provider = seed::provider(&db, "ingest-empty").create().await;

    let outcome = ingest_series(
        &db.pool,
        &scanned(provider, Vec::new()),
        &MatchingConfig::default(),
        &MetadataPriority::default(),
        &tankovault_domain::TermBlocklist::default(),
        &tankovault_domain::AdultTagSet::defaults(),
    )
    .await
    .expect("an empty listing still ingests the series itself");
    assert_eq!(outcome.new_chapters, Vec::<f64>::new());

    let direct = upsert_chapters(&db.pool, outcome.source_id, "/manga/solo-leveling", &[])
        .await
        .expect("the batch helper short-circuits");
    assert_eq!(direct, Vec::<f64>::new());
}

/// A chapter whose paywall lifts must lose its unlock time in the same write.
///
/// `chapters_unlocks_at_requires_early_access` (migration `0047`) rejects a free row carrying an
/// `unlocks_at`, and the whole chapter batch is one statement, so a re-scan that freed a chapter
/// but kept the stale date would abort the ingest of every chapter of that series. The upsert
/// overwrites both columns instead of coalescing them; this pins that, in both directions.
#[tokio::test]
async fn freeing_a_locked_chapter_clears_the_unlock_time_it_carried() {
    let db = TestDb::spawn().await;
    let provider = seed::provider(&db, "paywalled").create().await;
    let outcome = ingest_series(
        &db.pool,
        &scanned(provider, vec![chapter(1.0, None, "/c/1")]),
        &MatchingConfig::default(),
        &MetadataPriority::default(),
        &tankovault_domain::TermBlocklist::default(),
        &tankovault_domain::AdultTagSet::defaults(),
    )
    .await
    .expect("first ingest");

    let locked = ChapterUpsert {
        access: tankovault_domain::ChapterAccess::EarlyAccess,
        unlocks_at: Some(time::OffsetDateTime::now_utc() + time::Duration::days(7)),
        ..chapter(1.0, None, "/c/1")
    };
    upsert_chapters(
        &db.pool,
        outcome.source_id,
        "/manga/solo-leveling",
        &[locked],
    )
    .await
    .expect("the chapter goes behind the paywall");
    let (access, unlocks): (
        tankovault_domain::ChapterAccess,
        Option<time::OffsetDateTime>,
    ) = sqlx::query_as(
        "SELECT access, unlocks_at FROM chapters WHERE series_source_id = $1 AND number_milli = 10000",
    )
    .bind(outcome.source_id.as_uuid())
    .fetch_one(&db.pool)
    .await
    .expect("read back");
    assert_eq!(access, tankovault_domain::ChapterAccess::EarlyAccess);
    assert!(unlocks.is_some(), "the stated unlock time is stored");

    // The timer expires and the next scan reports it free. Coalescing here would leave the date
    // behind and the CHECK constraint would take the whole batch down with it.
    upsert_chapters(
        &db.pool,
        outcome.source_id,
        "/manga/solo-leveling",
        &[chapter(1.0, None, "/c/1")],
    )
    .await
    .expect("freeing the chapter must not violate the access CHECK");
    let (access, unlocks): (
        tankovault_domain::ChapterAccess,
        Option<time::OffsetDateTime>,
    ) = sqlx::query_as(
        "SELECT access, unlocks_at FROM chapters WHERE series_source_id = $1 AND number_milli = 10000",
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
/// Everything that screen shows derives from this list (the totals, the read and unread split,
/// the "next up" marker), so a paywalled chapter left in it puts a paywall link under "next up"
/// and makes the counts disagree with every other surface. An anonymous visitor
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
        &tankovault_domain::TermBlocklist::default(),
        &tankovault_domain::AdultTagSet::defaults(),
    )
    .await
    .expect("ingest");
    sqlx::query(
        "UPDATE chapters SET access = 'early_access', unlocks_at = now() + interval '7 days' \
         WHERE number_milli = 30000",
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

    // A stated unlock time that has passed frees the chapter for everyone with no rescan, the
    // rule the unread predicate applies and the reason `unlocks_at` is stored.
    sqlx::query(
        "UPDATE chapters SET unlocks_at = now() - interval '1 minute' WHERE number_milli = 30000",
    )
    .execute(&db.pool)
    .await
    .expect("expire the timer");
    assert_eq!(numbers(None).await, vec![3.0, 2.0, 1.0]);
}

async fn series_count(db: &TestDb) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM series")
        .fetch_one(&db.pool)
        .await
        .expect("count series")
}

/// Rescanning a source whose series has since been renamed must land in that series.
///
/// The bug: re-canonicalising every scan on the page title. Once `AniList` renames the series,
/// that title matches nothing, so each scan creates a fresh series, writes the page's metadata
/// onto it, and leaves it with no source, since the existing source is never re-pointed: dozens
/// of identical "0 chapters" cards per work, and `chapter.discovered` naming a series nobody
/// watches.
#[tokio::test]
async fn a_rescan_after_a_rename_does_not_create_an_empty_duplicate() {
    let db = TestDb::spawn().await;
    let provider = seed::provider(&db, "ingest-renamed").create().await;

    let first = ingest_series(
        &db.pool,
        &scanned(provider, vec![chapter(1.0, None, "/c/1")]),
        &MatchingConfig::default(),
        &MetadataPriority::default(),
        &tankovault_domain::TermBlocklist::default(),
        &tankovault_domain::AdultTagSet::defaults(),
    )
    .await
    .expect("first ingest");

    // What an `AniList` title win leaves behind: a canonical title the page no longer matches.
    sqlx::query("UPDATE series SET canonical_title = $2, normalized_title = $3 WHERE id = $1")
        .bind(first.series_id.as_uuid())
        .bind("Na Honjaman Level Up")
        .bind(normalize_title("Na Honjaman Level Up"))
        .execute(&db.pool)
        .await
        .expect("rename");

    let rescan = ingest_series(
        &db.pool,
        &scanned(
            provider,
            vec![chapter(1.0, None, "/c/1"), chapter(2.0, None, "/c/2")],
        ),
        &MatchingConfig::default(),
        &MetadataPriority::default(),
        &tankovault_domain::TermBlocklist::default(),
        &tankovault_domain::AdultTagSet::defaults(),
    )
    .await
    .expect("rescan");

    assert_eq!(
        rescan.series_id, first.series_id,
        "the scan belongs to the series its source is filed under"
    );
    assert_eq!(rescan.source_id, first.source_id);
    assert_eq!(rescan.new_chapters, vec![2.0]);
    assert_eq!(series_count(&db).await, 1, "no empty duplicate was created");
}

/// The first full scan of a catalogue stub files into the stub's series, whatever it is titled.
///
/// A listing and its series page often spell the work differently ("Return of the Mount Hua
/// Sect" against "Hwasangwihwan"). Canonicalising the page title would create a second series on
/// the first scan, while the stub's source and every chapter stay on the first.
#[tokio::test]
async fn a_stub_scanned_under_another_title_keeps_its_series() {
    let db = TestDb::spawn().await;
    let provider = seed::provider(&db, "ingest-stub").create().await;
    register_source_stub(
        &db.pool,
        provider,
        "/manga/solo-leveling",
        "I Level Up Alone",
        &MatchingConfig::default(),
    )
    .await
    .expect("register stub");
    let stub: uuid::Uuid = sqlx::query_scalar("SELECT id FROM series")
        .fetch_one(&db.pool)
        .await
        .expect("stub series");

    let outcome = ingest_series(
        &db.pool,
        &scanned(provider, vec![chapter(1.0, None, "/c/1")]),
        &MatchingConfig::default(),
        &MetadataPriority::default(),
        &tankovault_domain::TermBlocklist::default(),
        &tankovault_domain::AdultTagSet::defaults(),
    )
    .await
    .expect("first scan");

    assert_eq!(outcome.series_id.as_uuid(), stub);
    assert_eq!(series_count(&db).await, 1, "no empty duplicate was created");
}

/// **Two scans registering the same new paths at once leave no empty series behind.**
///
/// `register_chunk` creates a series per fresh path and then upserts the sources, and the upsert
/// keeps the series a concurrent scan already filed the path under. The series this scan created
/// for that path is left with no source, an orphan nothing would ever scan again. Each path must
/// end up with exactly one series.
///
/// The overlap is real concurrency, not staged: with no overlap this passes without the guard too,
/// so it can only ever fail for the right reason.
#[tokio::test]
async fn concurrent_registrations_of_one_path_leave_no_empty_series() {
    use tankovault_db::repo::catalog::register_source_stubs;

    let db = TestDb::spawn().await;
    let provider = seed::provider(&db, "ingest-race").create().await;
    let entries: Vec<(String, String)> = (0..200)
        .map(|i| (format!("/manga/race-{i}"), format!("Race Work {i}")))
        .collect();
    let borrowed: Vec<(&str, &str)> = entries
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();
    let policy = MatchingConfig::default();

    let (a, b) = tokio::join!(
        register_source_stubs(&db.pool, provider, &borrowed, &policy),
        register_source_stubs(&db.pool, provider, &borrowed, &policy),
    );
    a.expect("first scan");
    b.expect("second scan");

    let empty: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM series s \
          WHERE NOT EXISTS (SELECT 1 FROM series_sources ss WHERE ss.series_id = s.id)",
    )
    .fetch_one(&db.pool)
    .await
    .expect("count empty series");
    assert_eq!(empty, 0, "a lost registration race left an empty series");
    assert_eq!(series_count(&db).await, 200);
}

/// **A mirror listing attaches through a sibling source's listing title.**
///
/// Once `AniList` has renamed a series, a mirror's stub (listing `Legend of Star General`)
/// matches it only through an alias, which never attaches, so registration would create a new
/// series for the sweep to merge later. The first source's listing title is a primary title, and
/// the stub attaches to it.
#[tokio::test]
async fn a_mirror_stub_attaches_through_a_sibling_listing_title() {
    let db = TestDb::spawn().await;
    let first = seed::provider(&db, "mirror-a").create().await;
    let second = seed::provider(&db, "mirror-b").create().await;
    register_source_stub(
        &db.pool,
        first,
        "/manga/legend-of-star-general",
        "Legend of Star General",
        &MatchingConfig::default(),
    )
    .await
    .expect("first stub");
    let series: uuid::Uuid = sqlx::query_scalar("SELECT id FROM series")
        .fetch_one(&db.pool)
        .await
        .expect("series");
    // What an `AniList` title win leaves behind.
    sqlx::query("UPDATE series SET canonical_title = $2, normalized_title = $3 WHERE id = $1")
        .bind(series)
        .bind("Xing Jiang Chuanqi")
        .bind(normalize_title("Xing Jiang Chuanqi"))
        .execute(&db.pool)
        .await
        .expect("rename");

    register_source_stub(
        &db.pool,
        second,
        "/series/legend-of-star-general",
        "Legend of Star General",
        &MatchingConfig::default(),
    )
    .await
    .expect("mirror stub");

    assert_eq!(series_count(&db).await, 1, "the mirror created a series");
    let filed: Vec<uuid::Uuid> = sqlx::query_scalar("SELECT series_id FROM series_sources")
        .fetch_all(&db.pool)
        .await
        .expect("sources");
    assert_eq!(filed, vec![series, series]);
}

/// **A source that only shares an alias with a series files its aliases on a series of its own.**
///
/// The bug: filing the incoming source's alternative titles under the series it attached to lets
/// one weak attach pull the next work in through those names, until hub series absorb dozens of
/// different works each. An alias-only match does not attach at all, which keeps the incoming
/// aliases off the matched series.
#[tokio::test]
async fn an_alias_only_match_does_not_file_its_aliases_on_the_matched_series() {
    use tankovault_db::repo::catalog::list_series_titles;

    let db = TestDb::spawn().await;
    let provider = seed::provider(&db, "alias-filing").create().await;
    let mut existing = scanned(provider, vec![chapter(1.0, None, "/c/1")]);
    existing.source_path = "/manga/sword-art-online".to_owned();
    existing.provider_title = Some("Sword Art Online".to_owned());
    existing.meta.canonical_title = "Sword Art Online".to_owned();
    existing.meta.normalized_title = normalize_title("Sword Art Online");
    existing.alt_titles = vec![(
        "Sword Art Online Progressive".to_owned(),
        normalize_title("Sword Art Online Progressive"),
    )];
    let hub = ingest_series(
        &db.pool,
        &existing,
        &MatchingConfig::default(),
        &MetadataPriority::default(),
        &tankovault_domain::TermBlocklist::default(),
        &tankovault_domain::AdultTagSet::defaults(),
    )
    .await
    .expect("existing series");

    let mut incoming = scanned(provider, vec![chapter(1.0, None, "/c/1")]);
    incoming.source_path = "/manga/sao-progressive".to_owned();
    incoming.provider_title = Some("Sword Art Online Progressive".to_owned());
    incoming.meta.canonical_title = "Sword Art Online Progressive".to_owned();
    incoming.meta.normalized_title = normalize_title("Sword Art Online Progressive");
    incoming.alt_titles = vec![(
        "Kaiten Progressive".to_owned(),
        normalize_title("Kaiten Progressive"),
    )];
    let outcome = ingest_series(
        &db.pool,
        &incoming,
        &MatchingConfig::default(),
        &MetadataPriority::default(),
        &tankovault_domain::TermBlocklist::default(),
        &tankovault_domain::AdultTagSet::defaults(),
    )
    .await
    .expect("incoming series");

    assert_ne!(
        outcome.series_id, hub.series_id,
        "an alias hit alone attached"
    );
    let hub_titles = list_series_titles(&db.pool, hub.series_id)
        .await
        .expect("titles");
    assert!(
        !hub_titles.iter().any(|t| t.contains("Kaiten")),
        "the incoming source's alias reached the matched series: {hub_titles:?}"
    );
}
