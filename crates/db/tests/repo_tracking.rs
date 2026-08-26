//! Read progress: the one question this product answers five times over
//! (`crates/db/src/repo/tracking/`, TEST F-05).
//!
//! # Why this file exists
//!
//! "Has this user read this chapter?" is implemented **twice, independently**:
//! [`ReadProgress::covers`] decides it in Rust, and four SQL statements decide it inline because
//! `sqlx`'s checked macros need a string literal and will not expand `concat!`. A fifth caller —
//! the notifier — used to decide it a third way, by hand, from one of the two frontiers.
//!
//! Nothing compared them. Every one of those statements type-checks against the schema and
//! passes `cargo sqlx prepare --check` whether the predicate is right or inverted, so the only
//! thing that can catch a copy drifting is a test that drives both implementations over the same
//! seeded matrix and asserts they agree. That is
//! [`the_sql_and_the_rust_predicate_agree_on_every_chapter`], and it is the reason this file
//! exists — the audit calls it the single highest-value database test in the repository.
//!
//! # What the matrix is shaped to catch
//!
//! The two frontiers exist because a source can ship *part* releases (`4.5`, `4.75`) ahead of the
//! compiled whole chapter, and a part belongs **to** the chapter it floors to rather than
//! following it. That makes three regions where the two implementations can disagree, and the
//! catalogue below has a chapter in each:
//!
//! - a part *below* the whole frontier (`3.5` with the frontier at `3`) — read, because its
//!   chapter is read;
//! - a part *ahead* of the whole frontier (`4.5`, `4.75`, whose whole chapter `4` does not exist
//!   as a row at all) — decided by the part frontier alone;
//! - a gap (no chapter `5`) and a chapter far past the frontier (`10`), so "unread" cannot be
//!   satisfied by counting.
//!
//! Zero progress, no progress row at all, and a frontier past the last chapter are in the
//! matrix for the same reason: `0` is both this schema's "nothing read" sentinel and a
//! legitimate chapter number.
//!
//! # TRACK-1 — what the differential found
//!
//! Three of the four SQL copies carried only the whole-frontier half of the predicate
//! (`floor(c.number) > COALESCE(last_read_whole_number, 0)`), which is precisely the hand-roll
//! [`ReadProgress::covers`]'s own documentation forbids. The user-visible effect was a
//! continue-reading card that **could not be cleared**: read every part of an unreleased chapter
//! and the badge still claimed one unread, `next_number` pointed at a part already read, and
//! marking that part read again is a deliberate no-op in [`progress_mark_read`] — so the card
//! stayed, forever, while the feed on the same page correctly showed nothing. The watchlist badge
//! and the lifetime stats counted the same phantom. All four copies are now spelled identically;
//! the tests below fail if any of them regresses.
//!
//! Opt-in: gated behind the `integration` feature because it requires Docker.
#![cfg(feature = "integration")]

use tankovault_db::repo::matching::merge_series;
use tankovault_db::repo::tracking::{
    PinOutcome, ReadProgress, WatchlistFilter, WatchlistPage, continue_reading,
    early_access_opted_in, feed, is_sync_excluded, me_stats, progress_get_full, progress_mark_read,
    progress_mark_unread, progress_set, set_sync_excluded, watchers_for_series, watchlist_card,
    watchlist_list, watchlist_page, watchlist_set_pinned_source, watchlist_track_if_absent,
    watchlist_upsert,
};
use tankovault_db::repo::users::set_notification_prefs;
use tankovault_domain::{
    NotificationKind, NotificationPrefs, ProviderId, SeriesId, SeriesSourceId, UserId, WatchStatus,
};
use tankovault_test_support::{TestDb, seed};

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

/// The catalogue every differential test runs over.
///
/// Ascending, and deliberately irregular: `3.5` is a part of a chapter that exists, `4.5`/`4.75`
/// are parts of a whole chapter (`4`) that does **not** exist as a row, `5` is missing entirely,
/// `6.1` is a part of the second-to-last chapter, and `10` is far enough ahead that no frontier
/// in the matrix reaches it except the "past the end" one.
const CHAPTERS: &[f64] = &[1.0, 2.0, 3.0, 3.5, 4.5, 4.75, 6.0, 6.1, 10.0];

/// One row of the progress matrix: `None` means no `read_progress` row at all, which is a
/// different state from a row whose whole frontier is `0`.
struct Frontier {
    name: &'static str,
    progress: Option<(f64, Option<f64>)>,
}

/// Every progress state the two frontiers can express against [`CHAPTERS`], including the three
/// that make the whole-frontier-only predicate wrong.
///
/// Each `Some((whole, part))` upholds the §A.1 invariant (`part IS NULL OR floor(part) >= whole`);
/// a state that violated it would be testing something the write paths cannot produce.
const MATRIX: &[Frontier] = &[
    Frontier {
        name: "no progress row at all",
        progress: None,
    },
    Frontier {
        // `0` is the "nothing read" sentinel *and* chapter zero. No chapter zero exists here, so
        // this must answer exactly as "no row" does — if it did not, the `COALESCE(…, 0)` in the
        // SQL and the `Default` in Rust would have drifted apart.
        name: "frontier at the zero sentinel",
        progress: Some((0.0, None)),
    },
    Frontier {
        name: "mid-catalogue",
        progress: Some((2.0, None)),
    },
    Frontier {
        // The frontier sits on a chapter that has a part under it: `3.5` must read as *read*.
        name: "on a whole chapter that has a part under it",
        progress: Some((3.0, None)),
    },
    Frontier {
        // The state the two-scalar model exists for: chapter 4 is not out, but its first part is
        // and has been read. Only the part frontier can express this.
        name: "one part ahead of the whole frontier",
        progress: Some((3.0, Some(4.5))),
    },
    Frontier {
        // Every part of the unreleased chapter 4 has been read. Whole-frontier-only SQL reports
        // chapter 4 as unread here and offers `4.5` as the next chapter — TRACK-1.
        name: "every part of an unreleased chapter",
        progress: Some((3.0, Some(4.75))),
    },
    Frontier {
        name: "on the unreleased chapter's own number",
        progress: Some((4.0, None)),
    },
    Frontier {
        name: "on the second-to-last whole chapter",
        progress: Some((6.0, None)),
    },
    Frontier {
        // A part frontier that the whole frontier has already overtaken: redundant, and must not
        // change any answer. The write paths clear it, but a row like this can survive an
        // external-sync pull, so the read models have to tolerate it.
        name: "a part frontier the whole frontier already covers",
        progress: Some((6.0, Some(6.1))),
    },
    Frontier {
        name: "past the last chapter",
        progress: Some((99.0, None)),
    },
];

/// Ingest one series carrying `chapters` on `provider_id`.
async fn a_series(db: &TestDb, provider_id: ProviderId, title: &str, chapters: &[f64]) -> SeriesId {
    seed::series(db, provider_id, title)
        .chapters(chapters)
        .create()
        .await
}

/// Plant a progress state directly.
///
/// Deliberately not routed through [`progress_mark_read`]: this suite is testing the *read*
/// models against [`ReadProgress::covers`], and driving the fixture through the write path would
/// make a bug there hide a bug here. The write path gets its own tests further down.
async fn set_frontiers(
    db: &TestDb,
    user: UserId,
    series: SeriesId,
    state: Option<(f64, Option<f64>)>,
) {
    sqlx::query("DELETE FROM read_progress WHERE user_id = $1 AND series_id = $2")
        .bind(user.as_uuid())
        .bind(series.as_uuid())
        .execute(&db.pool)
        .await
        .expect("clear progress");
    if let Some((whole, part)) = state {
        sqlx::query(
            "INSERT INTO read_progress \
                 (user_id, series_id, last_read_whole_number, last_read_part_number) \
             VALUES ($1,$2,$3::float8::numeric(10,4),$4::float8::numeric(10,4))",
        )
        .bind(user.as_uuid())
        .bind(series.as_uuid())
        .bind(whole)
        .bind(part)
        .execute(&db.pool)
        .await
        .expect("seed progress");
    }
}

/// The Rust side of the differential, from the same state the SQL sees.
fn rust_progress(state: Option<(f64, Option<f64>)>) -> ReadProgress {
    match state {
        Some((whole, part)) => ReadProgress {
            last_read_whole_number: whole,
            last_read_part_number: part,
        },
        // A missing row is what `COALESCE(last_read_whole_number, 0)` resolves to in SQL.
        None => ReadProgress::default(),
    }
}

/// Every catalogue chapter [`ReadProgress::covers`] reports as unread, in ascending order.
fn rust_unread(progress: ReadProgress) -> Vec<f64> {
    CHAPTERS
        .iter()
        .copied()
        .filter(|&n| !progress.covers(n))
        .collect()
}

/// Distinct **whole** chapters with unread content — what every `unread` badge counts.
fn rust_unread_whole_count(progress: ReadProgress) -> i64 {
    let mut wholes: Vec<f64> = rust_unread(progress).iter().map(|n| n.floor()).collect();
    // `CHAPTERS` is ascending, so its floors are too and adjacent dedup is a full dedup.
    wholes.dedup();
    i64::try_from(wholes.len()).expect("catalogue fits in i64")
}

/// Distinct **whole** chapters at or below the whole frontier — the progress bar's numerator.
///
/// Deliberately *not* the complement of [`rust_unread_whole_count`]: a whole chapter above the
/// frontier whose every part the part frontier covers is in neither set, so `read + unread` can
/// be less than the total. The bar under-reads in that case, which is the honest direction —
/// the alternative is claiming a chapter is read because nothing in it is unread.
fn rust_read_whole_count(progress: ReadProgress) -> i64 {
    let mut wholes: Vec<f64> = CHAPTERS
        .iter()
        .map(|n| n.floor())
        .filter(|&n| n <= progress.last_read_whole_number)
        .collect();
    wholes.dedup();
    i64::try_from(wholes.len()).expect("catalogue fits in i64")
}

// ---------------------------------------------------------------------------
// The differential
// ---------------------------------------------------------------------------

/// **The one that matters.** Every SQL statement that decides "is this chapter unread?" must
/// return exactly the chapters [`ReadProgress::covers`] says are unread, for every progress
/// state the two frontiers can express.
///
/// Deleting this leaves five implementations of one predicate with nothing comparing them. The
/// failure mode it guards is silent in every other kind of test: each statement compiles, passes
/// `sqlx prepare --check`, and returns a plausible page — just not the same page as the Rust
/// definition, so the feed, the continue rail, the watchlist badge and the lifetime stats
/// disagree with each other about the same chapter.
///
/// It found TRACK-1 (see the module docs) on its first run: three of the four copies were missing
/// the part-frontier clause entirely.
#[tokio::test]
async fn the_sql_and_the_rust_predicate_agree_on_every_chapter() {
    let db = TestDb::spawn().await;
    let user = seed::user(&db, "reader").create().await;
    let provider = seed::provider(&db, "alpha").create().await;
    let series = a_series(&db, provider, "Berserk", CHAPTERS).await;
    watchlist_upsert(&db.pool, user, series, WatchStatus::Reading, true)
        .await
        .expect("watchlist");

    for state in MATRIX {
        set_frontiers(&db, user, series, state.progress).await;
        let progress = rust_progress(state.progress);
        let expected = rust_unread(progress);
        let expected_count = rust_unread_whole_count(progress);

        // 1. The feed lists exactly the unread chapters.
        let mut listed: Vec<f64> = feed(&db.pool, user, 100)
            .await
            .expect("feed")
            .iter()
            .map(|item| item.chapter_number)
            .collect();
        listed.sort_unstable_by(f64::total_cmp);
        assert_eq!(
            listed, expected,
            "feed disagrees with ReadProgress::covers ({})",
            state.name
        );

        // 2. The continue-reading card's badge and next-chapter pointer. `unread > 0` is a
        //    condition of the row existing at all, so "nothing unread" means "no card".
        let cards = continue_reading(&db.pool, user).await.expect("continue");
        let card = cards.iter().find(|c| c.series_id == series);
        match card {
            Some(card) => {
                assert_eq!(
                    card.unread, expected_count,
                    "continue_reading unread count ({})",
                    state.name
                );
                assert_eq!(
                    card.next_number,
                    expected.first().copied(),
                    "continue_reading next_number must be the first *unread* chapter ({})",
                    state.name
                );
            }
            None => assert_eq!(
                expected_count, 0,
                "continue_reading dropped a series with unread chapters ({})",
                state.name
            ),
        }

        // 3. The watchlist badge. `limit` is raised past the page default so the assertion is
        //    about the predicate rather than about which page the series landed on.
        let page = watchlist_page(
            &db.pool,
            user,
            &WatchlistFilter {
                limit: 1000,
                ..WatchlistFilter::default()
            },
        )
        .await
        .expect("watchlist");
        let card = page
            .items
            .iter()
            .find(|c| c.series_id == series)
            .expect("the watched series");
        assert_eq!(
            card.unread, expected_count,
            "watchlist_page unread count ({})",
            state.name
        );
        // `next_unread` is a fifth copy of the predicate — a `LEFT JOIN LATERAL … LIMIT 1`
        // rather than a `count(… ) FILTER`, so it can drift from the badge above it
        // independently. A row reading "3 unread · next Ch 4" while the feed opens Ch 5 is
        // exactly the TRACK-1 shape, one column over.
        assert_eq!(
            card.next_unread.as_ref().map(|n| n.number),
            expected.first().copied(),
            "watchlist_page next_unread must be the first unread chapter ({})",
            state.name
        );
        assert_eq!(
            card.read_count,
            rust_read_whole_count(progress),
            "watchlist_page read_count ({})",
            state.name
        );
        assert!(
            card.read_count + card.unread <= card.total_chapters,
            "read + unread must never exceed the denominator they are drawn over ({})",
            state.name
        );

        // 4. The lifetime stats.
        let stats = me_stats(&db.pool, user).await.expect("stats");
        assert_eq!(
            stats.unread, expected_count,
            "me_stats unread count ({})",
            state.name
        );
    }
}

/// Keyset paging must visit every row exactly once, under every order the list offers.
///
/// # The bug this exists to stop
///
/// The seek predicate and the `ORDER BY` are two spellings of one ordering, and nothing but this
/// test compares them. Get the direction, the `NULLS LAST` arm or the id tiebreaker wrong in one
/// of them and the pages still come back full of plausible rows — just with one row repeated
/// across a page boundary and another never shown at all. That is invisible in a page-sized
/// assertion and invisible in production until a reader notices a title missing from a list they
/// scrolled past.
///
/// The fixture is deliberately degenerate: `limit` is 2 against 7 series, several of which tie
/// on `unread` and on `progress` (so the tiebreaker is load-bearing rather than decorative), and
/// two carry no chapters at all (so every order has `NULL` keys to place last).
#[tokio::test]
async fn keyset_pages_visit_every_row_exactly_once() {
    use tankovault_db::repo::tracking::{WatchlistOrder, WatchlistSort};

    let db = TestDb::spawn().await;
    let user = seed::user(&db, "pager").create().await;
    let provider = seed::provider(&db, "alpha").create().await;

    // Two with no chapters, three tied on chapter count, two distinct.
    let fixture: &[(&str, &[f64])] = &[
        ("Akira", &[]),
        ("Berserk", &[1.0, 2.0]),
        ("Chainsaw Man", &[1.0, 2.0]),
        ("Dorohedoro", &[1.0, 2.0]),
        ("Eden", &[1.0]),
        ("Frieren", &[1.0, 2.0, 3.0]),
        ("Goodnight Punpun", &[]),
    ];
    for (title, chapters) in fixture {
        let series = a_series(&db, provider, title, chapters).await;
        watchlist_upsert(&db.pool, user, series, WatchStatus::Reading, true)
            .await
            .expect("watchlist");
    }

    for sort in [
        WatchlistSort::Released,
        WatchlistSort::Unread,
        WatchlistSort::Added,
        WatchlistSort::Title,
        WatchlistSort::Progress,
    ] {
        for order in [WatchlistOrder::Desc, WatchlistOrder::Asc] {
            let base = WatchlistFilter {
                sort,
                order,
                limit: 1000,
                ..WatchlistFilter::default()
            };
            let whole: Vec<SeriesId> = watchlist_page(&db.pool, user, &base)
                .await
                .expect("unpaged")
                .items
                .iter()
                .map(|c| c.series_id)
                .collect();

            let mut walked = Vec::new();
            let mut cursor = None;
            // Bounded so a seek predicate that never advances fails as a wrong list rather than
            // hanging the suite.
            for _ in 0..fixture.len() + 2 {
                let page = watchlist_page(
                    &db.pool,
                    user,
                    &WatchlistFilter {
                        limit: 2,
                        cursor: cursor.clone(),
                        ..base.clone()
                    },
                )
                .await
                .expect("keyset page");
                walked.extend(page.items.iter().map(|c| c.series_id));
                match page.next_cursor {
                    Some(next) => cursor = Some(next),
                    None => break,
                }
            }

            assert_eq!(
                walked,
                whole,
                "keyset walk diverges from the unpaged order for {:?}/{:?}",
                sort.as_token(),
                order.as_token(),
            );
        }
    }
}

/// The summary counts every tracked title, whatever the list is filtered to.
///
/// It exists precisely because the list's own `counts` cannot answer this: those keep the
/// search, recency and source arms, so a reader who has typed into the filter box would see a
/// library-size badge that shrinks as they type.
#[tokio::test]
async fn the_summary_ignores_the_filters_the_list_counts_apply() {
    use tankovault_db::repo::tracking::watchlist_summary;

    let db = TestDb::spawn().await;
    let user = seed::user(&db, "summariser").create().await;
    let provider = seed::provider(&db, "alpha").create().await;

    let reading = a_series(&db, provider, "Berserk", &[1.0, 2.0]).await;
    let dropped = a_series(&db, provider, "Claymore", &[1.0]).await;
    watchlist_upsert(&db.pool, user, reading, WatchStatus::Reading, true)
        .await
        .expect("watchlist");
    watchlist_upsert(&db.pool, user, dropped, WatchStatus::Dropped, true)
        .await
        .expect("watchlist");

    let summary = watchlist_summary(&db.pool, user).await.expect("summary");
    assert_eq!(summary.counts.all, 2);
    assert_eq!(summary.counts.reading, 1);
    assert_eq!(summary.counts.dropped, 1);
    assert_eq!(summary.unread_total, 3, "two chapters plus one");

    // The same account, seen through a search that matches one title, must not change it.
    let filtered = watchlist_page(
        &db.pool,
        user,
        &WatchlistFilter {
            query: Some("Berserk".into()),
            ..WatchlistFilter::default()
        },
    )
    .await
    .expect("filtered");
    assert_eq!(filtered.counts.all, 1, "list counts follow the search");
    assert_eq!(
        watchlist_summary(&db.pool, user)
            .await
            .expect("summary")
            .counts
            .all,
        2,
        "the summary does not",
    );
}

/// One provider carrying a series twice is **one** carrier in the `Sources` column.
///
/// `series_sources` is unique on `(provider_id, source_path)`, not on `(series_id,
/// provider_id)`, so a provider can hold the same series under several paths — legitimately for
/// a colour edition, and in bulk when a scan mis-attaches. The column's query emitted a row per
/// source, so the ledger repeated one carrier's monogram across all four tiles and then
/// overflowed into a `+n` counting paths: a live catalogue had 309 rows and 3 providers on one
/// series, rendered as `+305`. It also disagreed with `source_count` on the same row, which has
/// always been `count(DISTINCT provider_id)`.
#[tokio::test]
async fn a_provider_carrying_a_series_twice_is_one_source() {
    let db = TestDb::spawn().await;
    let user = seed::user(&db, "collector").create().await;
    let alpha = seed::provider(&db, "alpha").create().await;
    let beta = seed::provider(&db, "beta").create().await;

    let series = a_series(&db, alpha, "Vagabond", &[1.0, 2.0]).await;
    // A second path on the *same* provider, ranked above the first so the survivor is the one
    // `preferred_source_name` names — the flag and the submeta have to agree.
    add_source(&db, series, alpha, "/vagabond-colored", 99).await;
    // ...and a genuinely different carrier, which must survive as its own tile.
    add_source(&db, series, beta, "/vagabond", 5).await;

    watchlist_upsert(&db.pool, user, series, WatchStatus::Reading, true)
        .await
        .expect("watchlist");

    let page = watchlist_page(&db.pool, user, &WatchlistFilter::default())
        .await
        .expect("watchlist");
    let card = page.items.first().expect("one row");

    let codes: Vec<&str> = card.sources.iter().map(|s| s.code.as_str()).collect();
    assert_eq!(
        codes,
        ["alpha", "beta"],
        "one tile per provider, best first"
    );
    assert_eq!(
        card.source_count,
        i64::try_from(card.sources.len()).expect("small"),
        "the tiles and the count must describe the same set",
    );
    assert!(
        card.sources[0].preferred,
        "the best-ranked carrier is tinted"
    );
    assert!(!card.sources[1].preferred, "and only that one");
}

/// Attach an extra source row to an existing series, as a second scan of the same provider does.
async fn add_source(
    db: &TestDb,
    series: SeriesId,
    provider: ProviderId,
    source_path: &str,
    chapter_count: i32,
) {
    sqlx::query(
        "INSERT INTO series_sources \
             (series_id, provider_id, source_path, chapter_count, last_scanned_at) \
         VALUES ($1, $2, $3, $4, now())",
    )
    .bind(series.as_uuid())
    .bind(provider.as_uuid())
    .bind(source_path)
    .bind(chapter_count)
    .execute(&db.pool)
    .await
    .expect("seed extra source");
}

/// A merged series lists each chapter **once** in the feed, resolved to its preferred carrier.
///
/// The bug: a merge re-parents the absorbed series' sources onto the survivor, so from then on
/// one chapter legitimately exists as two `chapters` rows carrying the same number. The feed
/// emitted a row per source, so Home showed the merged series twice, each row claiming the full
/// count — while the "New chapters" tile directly above them, `continue_reading`'s badge and the
/// watchlist ledger all count `DISTINCT number_milli / 10000` and said it once, and the notifier
/// claims one announcement per `(user, series, chapter)`. The duplicates also spent the `limit`,
/// so a watchlist of merged series came back as half a feed.
#[tokio::test]
async fn a_merged_series_lists_each_chapter_once_in_the_feed() {
    let db = TestDb::spawn().await;
    let user = seed::user(&db, "reader").create().await;
    let alpha = seed::provider(&db, "alpha").create().await;
    let beta = seed::provider(&db, "beta").create().await;

    // Two catalogue entries an operator judged to be one work. Deliberately unlike titles: the
    // point is the post-merge shape, and letting the matcher fold them at ingest instead would
    // leave nothing for `merge_series` to move.
    let keep = a_series(&db, alpha, "Vinland Saga", &[1.0, 2.0, 3.0]).await;
    let absorbed = a_series(&db, beta, "Historie", &[2.0, 3.0, 4.0]).await;
    assert_ne!(keep, absorbed, "the fixture needs two series to merge");
    merge_series(&db.pool, keep, absorbed, None, "merged")
        .await
        .expect("merge");

    // Beta is the richer carrier, so it is the source the ledger's `preferred_source_name`
    // names — and therefore the one whose link every shared chapter must resolve to.
    prefer_source(&db, keep, beta, 99).await;
    prefer_source(&db, keep, alpha, 3).await;

    watchlist_upsert(&db.pool, user, keep, WatchStatus::Reading, true)
        .await
        .expect("watchlist");

    // Both the shape and the carrier in one assertion: chapter 1 exists only on alpha, and the
    // two both sources hold resolve to beta because beta is the preferred one.
    let items = feed(&db.pool, user, 100).await.expect("feed");
    let mut carried: Vec<(f64, &str)> = items
        .iter()
        .map(|item| (item.chapter_number, item.provider_slug.as_str()))
        .collect();
    carried.sort_unstable_by(|left, right| left.0.total_cmp(&right.0));
    assert_eq!(
        carried,
        vec![(1.0, "alpha"), (2.0, "beta"), (3.0, "beta"), (4.0, "beta")],
        "one row per chapter, each opening on the preferred source that holds it"
    );
}

/// Rank one of a series' sources by giving it a chapter count, as a scan does.
async fn prefer_source(db: &TestDb, series: SeriesId, provider: ProviderId, chapter_count: i32) {
    sqlx::query(
        "UPDATE series_sources SET chapter_count = $3 WHERE series_id = $1 AND provider_id = $2",
    )
    .bind(series.as_uuid())
    .bind(provider.as_uuid())
    .bind(chapter_count)
    .execute(&db.pool)
    .await
    .expect("rank a source");
}

/// The notifier's "already read?" filter must be [`ReadProgress::covers`], not a comparison
/// against one frontier.
///
/// `watchers_for_series` returns the frontiers rather than a verdict, so the judgement is made in
/// `services/notifier`. It used to be written there as `chapter_number > last_read_number` over
/// the whole frontier alone, which announced every part release of an already-read chapter as
/// new — chapter `152.5` mailed out to a user who had finished `152`. This test drives the same
/// data through `covers` and asserts the two frontiers survive the round trip, which is what the
/// notifier needs to make the call at all: before the fix the part frontier was not even
/// selected.
#[tokio::test]
async fn the_notifier_sees_both_frontiers_and_they_decide_as_covers_does() {
    let db = TestDb::spawn().await;
    let user = seed::user(&db, "watcher").create().await;
    let quiet = seed::user(&db, "quiet").create().await;
    let provider = seed::provider(&db, "alpha").create().await;
    let series = a_series(&db, provider, "Berserk", CHAPTERS).await;
    watchlist_upsert(&db.pool, user, series, WatchStatus::Reading, true)
        .await
        .expect("watchlist");
    // `notify = false` must keep a user out of the fan-out entirely; otherwise a preference
    // toggle silently does nothing.
    watchlist_upsert(&db.pool, quiet, series, WatchStatus::Reading, false)
        .await
        .expect("watchlist");

    for state in MATRIX {
        set_frontiers(&db, user, series, state.progress).await;
        let watchers = watchers_for_series(&db.pool, series)
            .await
            .expect("watchers");
        assert_eq!(
            watchers.len(),
            1,
            "only the opted-in watcher is returned ({})",
            state.name
        );
        let watcher = &watchers[0];
        assert_eq!(watcher.user_id, user);
        assert_eq!(
            watcher.progress.is_some(),
            state.progress.is_some(),
            "a missing progress row must arrive as None, not as a zero frontier ({})",
            state.name
        );

        let progress = watcher.progress.unwrap_or_default();
        let notified: Vec<f64> = CHAPTERS
            .iter()
            .copied()
            .filter(|&n| !progress.covers(n))
            .collect();
        assert_eq!(
            notified,
            rust_unread(rust_progress(state.progress)),
            "the frontiers did not survive the round trip ({})",
            state.name
        );
    }
}

/// A watcher arrives with the watchlist status and the preference document that decide delivery.
///
/// The notifier used to consult `watchlist_entries.notify` alone, so a series the reader had
/// *dropped* kept notifying — and the account panel's three toggles were stored in a free-form
/// blob nothing ever read. Both inputs have to come back on the row, or the filter cannot be made
/// at all; a document this build cannot parse must arrive as the defaults rather than failing the
/// whole fan-out.
#[tokio::test]
async fn a_watcher_carries_its_status_and_preferences() {
    let db = TestDb::spawn().await;
    let reader = seed::user(&db, "dropper").create().await;
    let provider = seed::provider(&db, "alpha").create().await;
    let series = a_series(&db, provider, "Berserk", CHAPTERS).await;
    watchlist_upsert(&db.pool, reader, series, WatchStatus::Dropped, true)
        .await
        .expect("watchlist");

    let watchers = watchers_for_series(&db.pool, series)
        .await
        .expect("watchers");
    assert_eq!(watchers.len(), 1);
    assert_eq!(watchers[0].status, WatchStatus::Dropped);
    assert!(
        !watchers[0]
            .prefs
            .allows(NotificationKind::NewChapter, WatchStatus::Dropped),
        "a dropped series is muted by the shipped defaults"
    );

    let mut prefs = NotificationPrefs::default();
    prefs.watch_status.dropped = true;
    set_notification_prefs(&db.pool, reader, &prefs)
        .await
        .expect("save prefs");
    let watchers = watchers_for_series(&db.pool, series)
        .await
        .expect("watchers");
    assert!(
        watchers[0]
            .prefs
            .allows(NotificationKind::NewChapter, WatchStatus::Dropped),
        "opting back in reaches the fan-out"
    );

    sqlx::query("UPDATE users SET notification_prefs = '\"not a document\"' WHERE id = $1")
        .bind(reader.as_uuid())
        .execute(&db.pool)
        .await
        .expect("store an unparseable document");
    let watchers = watchers_for_series(&db.pool, series)
        .await
        .expect("watchers");
    assert_eq!(
        watchers[0].prefs,
        NotificationPrefs::default(),
        "an unparseable document decodes to the defaults, it does not drop the watcher"
    );
}

/// `me_stats.unread` counts `(series, whole chapter)` pairs, not whole chapters.
///
/// The subquery is `SELECT DISTINCT w.series_id, floor(c.number)`. Dropping `w.series_id` from
/// that `DISTINCT` compiles, type-checks, and collapses chapter 1 of every watched series into
/// one row — so a user tracking twenty series with the same numbering sees the unread count of
/// one of them. Nothing else in the codebase reads that subquery.
#[tokio::test]
async fn me_stats_counts_unread_chapters_per_series_not_globally() {
    let db = TestDb::spawn().await;
    let user = seed::user(&db, "reader").create().await;
    let provider = seed::provider(&db, "alpha").create().await;
    // Identical numbering on purpose: the two series' floors overlap completely.
    let first = a_series(&db, provider, "Berserk", &[1.0, 2.0, 3.0]).await;
    let second = a_series(&db, provider, "Vinland Saga", &[1.0, 2.0, 3.0]).await;
    for series in [first, second] {
        watchlist_upsert(&db.pool, user, series, WatchStatus::Reading, true)
            .await
            .expect("watchlist");
    }

    let stats = me_stats(&db.pool, user).await.expect("stats");
    assert_eq!(stats.tracking, 2);
    assert_eq!(
        stats.unread, 6,
        "three unread chapters in each of two series"
    );

    progress_set(&db.pool, user, first, 3.0)
        .await
        .expect("set progress");
    let stats = me_stats(&db.pool, user).await.expect("stats");
    assert_eq!(
        stats.unread, 3,
        "finishing one series leaves the other's three"
    );
    assert_eq!(
        stats.chapters_read, 3,
        "chapters_read sums the whole frontiers across series"
    );
}

// ---------------------------------------------------------------------------
// The write path — §A.3's transitions
// ---------------------------------------------------------------------------

async fn frontiers(db: &TestDb, user: UserId, series: SeriesId) -> (f64, Option<f64>) {
    let progress = progress_get_full(&db.pool, user, series)
        .await
        .expect("read progress")
        .expect("a progress row");
    (
        progress.last_read_whole_number,
        progress.last_read_part_number,
    )
}

/// Marking a *part* of an already-read whole chapter changes nothing.
///
/// The pair to [`ReadProgress::covers`]'s `parts_of_an_already_read_whole_chapter_are_read`: the
/// read model says `3.5` is read, so the write path must agree that marking it is a no-op. If it
/// instead advanced the part frontier, the §A.1 invariant (`floor(part) >= whole`) would break and
/// `progress_set`'s stale-part `CASE` would start clearing frontiers it should keep.
#[tokio::test]
async fn marking_a_part_of_a_read_whole_chapter_is_a_noop() {
    let db = TestDb::spawn().await;
    let user = seed::user(&db, "reader").create().await;
    let provider = seed::provider(&db, "alpha").create().await;
    let series = a_series(&db, provider, "Berserk", CHAPTERS).await;

    progress_set(&db.pool, user, series, 3.0)
        .await
        .expect("set progress");
    progress_mark_read(&db.pool, user, series, 3.5)
        .await
        .expect("mark read");
    assert_eq!(frontiers(&db, user, series).await, (3.0, None));
}

/// The whole frontier never retreats because an earlier chapter was marked read.
///
/// `progress_mark_read` reads, decides in Rust, and writes both frontiers unconditionally, so a
/// missing `max` is a plain overwrite: opening chapter 1 of a series you have finished would reset
/// your progress to 1 and mark 200 chapters unread. There is no undo for that.
#[tokio::test]
async fn progress_mark_read_is_monotonic_in_the_whole_frontier() {
    let db = TestDb::spawn().await;
    let user = seed::user(&db, "reader").create().await;
    let provider = seed::provider(&db, "alpha").create().await;
    let series = a_series(&db, provider, "Berserk", CHAPTERS).await;

    progress_mark_read(&db.pool, user, series, 6.0)
        .await
        .expect("mark read");
    for earlier in [1.0, 2.0, 3.0] {
        progress_mark_read(&db.pool, user, series, earlier)
            .await
            .expect("mark read");
        assert_eq!(
            frontiers(&db, user, series).await,
            (6.0, None),
            "marking chapter {earlier} must not move the frontier back"
        );
    }
}

/// A part ahead of the whole frontier advances the part frontier only, and the whole frontier
/// overtaking it clears it.
///
/// Two claims in one, because they are the same invariant from both sides: the part frontier
/// exists only to describe reading *ahead* of the whole one, so it must never be the thing that
/// records whole-chapter progress, and it must not survive as a stale value once the whole
/// frontier has passed it. A surviving stale part violates §A.1 and makes `covers` answer from a
/// frontier that no longer means anything.
///
/// The parts here belong to chapter `4` and the frontier sits on `3`, i.e. the part is one
/// chapter ahead — so the whole frontier has nothing below `4` left to catch up to and genuinely
/// does not move. Marking a part *further* ahead does move it; that is the test below.
#[tokio::test]
async fn a_part_ahead_advances_only_the_part_frontier_and_is_cleared_when_overtaken() {
    let db = TestDb::spawn().await;
    let user = seed::user(&db, "reader").create().await;
    let provider = seed::provider(&db, "alpha").create().await;
    let series = a_series(&db, provider, "Berserk", CHAPTERS).await;

    progress_set(&db.pool, user, series, 3.0)
        .await
        .expect("set progress");
    progress_mark_read(&db.pool, user, series, 4.5)
        .await
        .expect("mark read");
    assert_eq!(frontiers(&db, user, series).await, (3.0, Some(4.5)));

    // A second, further part moves the part frontier and still not the whole one.
    progress_mark_read(&db.pool, user, series, 4.75)
        .await
        .expect("mark read");
    assert_eq!(frontiers(&db, user, series).await, (3.0, Some(4.75)));

    // Reading whole chapter 6 subsumes every 4.x, so the part frontier goes.
    progress_mark_read(&db.pool, user, series, 6.0)
        .await
        .expect("mark read");
    assert_eq!(frontiers(&db, user, series).await, (6.0, None));
}

/// Marking a part release far ahead of the whole frontier drags the whole frontier up with it.
///
/// The bug: `progress_mark_read` treated a part purely as part-frontier business, so marking
/// `6.1` from a frontier of `1` wrote `(1, 6.1)`. Two things were then wrong at once. Locally the
/// frontier contradicted itself — chapters `2`..`4` reported unread while `6.1` reported read,
/// which the §A.1 "marking a chapter read means read through here" contract forbids. And
/// externally, the `AniList` push sends `last_read_whole_number` and nothing else (§B.5), because a
/// provider with no concept of parts can be told nothing else — so it kept receiving `1` no
/// matter how many part releases were read on top. Deriving the number at the push site would
/// have fixed the second and left the first.
///
/// The catch-up target is the catalogue's, not `floor(number) - 1`: chapter `5` does not exist
/// here, and chapter `4` exists only as the parts `4.5`/`4.75`, so the answer is `4` — every part
/// of chapter 4 that was ever published has been read.
#[tokio::test]
async fn marking_a_part_far_ahead_catches_the_whole_frontier_up() {
    let db = TestDb::spawn().await;
    let user = seed::user(&db, "reader").create().await;
    let provider = seed::provider(&db, "alpha").create().await;
    let series = a_series(&db, provider, "Berserk", CHAPTERS).await;

    progress_set(&db.pool, user, series, 1.0)
        .await
        .expect("set progress");
    progress_mark_read(&db.pool, user, series, 6.1)
        .await
        .expect("mark read");
    assert_eq!(
        frontiers(&db, user, series).await,
        (4.0, Some(6.1)),
        "reading 6.1 asserts everything below chapter 6, and 4 is the highest the catalogue holds"
    );

    // Chapter 6 itself stays unread: `6.1` is a fragment shipped *ahead of* it, so it says
    // nothing about the rest of chapter 6. Catching up to `6` here would mark a chapter read
    // that nobody read.
    let p = progress_get_full(&db.pool, user, series)
        .await
        .expect("read progress")
        .expect("a progress row");
    assert!(p.covers(4.75), "every part of chapter 4 is covered");
    assert!(!p.covers(6.0), "chapter 6 is not read");
    assert!(p.covers(6.1), "the part that was marked is read");
}

/// The whole frontier's catch-up never *retreats* it, and never overshoots a part already read.
///
/// The catch-up target comes from the catalogue rather than from the current frontier, so
/// applying it unconditionally is how marking a chapter read walks progress backwards — the same
/// shape as TRACK-2 on the un-read side, in the other direction. Marking `4.5` read while the
/// frontier sits at `6` must leave `6` alone, not reset it to `4`.
#[tokio::test]
async fn the_whole_frontier_catch_up_is_monotonic() {
    let db = TestDb::spawn().await;
    let user = seed::user(&db, "reader").create().await;
    let provider = seed::provider(&db, "alpha").create().await;
    let series = a_series(&db, provider, "Berserk", CHAPTERS).await;

    progress_set(&db.pool, user, series, 6.0)
        .await
        .expect("set progress");
    // Below the frontier: covered already, so the whole branch is not even reached.
    progress_mark_read(&db.pool, user, series, 4.5)
        .await
        .expect("mark read");
    assert_eq!(frontiers(&db, user, series).await, (6.0, None));

    // Far ahead of the frontier, but nothing exists between them: chapters `7`..`9` are absent
    // and chapter `10` is excluded (it is the chapter `10.5` is a part of), so the catch-up
    // target is `6` — the frontier itself — and the whole frontier stays put.
    progress_mark_read(&db.pool, user, series, 10.5)
        .await
        .expect("mark read");
    assert_eq!(frontiers(&db, user, series).await, (6.0, Some(10.5)));
}

/// `progress_set` clears a part frontier its new whole frontier covers and keeps one still ahead.
///
/// This is the path external sync takes when it adopts a remote integer progress, and the `CASE`
/// is the only thing standing between that and a §A.1 violation. Clearing unconditionally would
/// throw away genuine reading ahead of the pulled value; never clearing would leave a stale part
/// behind a frontier that has passed it.
#[tokio::test]
async fn progress_set_clears_only_a_part_frontier_it_covers() {
    let db = TestDb::spawn().await;
    let user = seed::user(&db, "reader").create().await;
    let provider = seed::provider(&db, "alpha").create().await;
    let series = a_series(&db, provider, "Berserk", CHAPTERS).await;

    progress_set(&db.pool, user, series, 3.0)
        .await
        .expect("set progress");
    progress_mark_read(&db.pool, user, series, 6.1)
        .await
        .expect("mark read");
    // The whole frontier catches up to `4` on the way (the highest chapter below 6 that the
    // catalogue holds — `5` is missing and `4.5`/`4.75` floor to `4`); see
    // `marking_a_part_far_ahead_catches_the_whole_frontier_up`. What this test is about starts
    // below.
    assert_eq!(frontiers(&db, user, series).await, (4.0, Some(6.1)));

    // Still behind the part: it survives.
    progress_set(&db.pool, user, series, 4.0)
        .await
        .expect("set progress");
    assert_eq!(frontiers(&db, user, series).await, (4.0, Some(6.1)));

    // Now level with it (`floor(6.1) <= 6`): it is stale and goes.
    progress_set(&db.pool, user, series, 6.0)
        .await
        .expect("set progress");
    assert_eq!(frontiers(&db, user, series).await, (6.0, None));
}

/// Un-reading a part of an already-read whole chapter retreats **both** frontiers, to the
/// chapters the catalogue actually holds.
///
/// The branch this covers carries the longest comment in `progress.rs` for a reason: the two
/// frontiers cannot express "3.5 unread, 3 read", so un-reading `3.5` has to un-read chapter 3
/// too and then pick up whatever part is still read underneath. Without the branch the write is a
/// silent no-op — the button appears to work and nothing changes. The retreat targets come from
/// `prev_whole_below`/`prev_part_below`, which query the *catalogue*, so this also pins that
/// un-reading lands on a chapter that exists rather than on `number - 1`.
#[tokio::test]
async fn un_reading_a_part_below_the_frontier_retreats_both_frontiers() {
    let db = TestDb::spawn().await;
    let user = seed::user(&db, "reader").create().await;
    let provider = seed::provider(&db, "alpha").create().await;
    let series = a_series(&db, provider, "Berserk", CHAPTERS).await;

    progress_set(&db.pool, user, series, 6.0)
        .await
        .expect("set progress");
    progress_mark_unread(&db.pool, user, series, 3.5)
        .await
        .expect("mark unread");
    // Chapter 2 is the highest whole chapter below 3 that exists, and no part sits between
    // chapter 2 and 3.5 in the catalogue.
    assert_eq!(frontiers(&db, user, series).await, (2.0, None));

    // Un-reading a whole chapter retreats to the previous *existing* whole chapter — 5 is
    // missing, so un-reading 6 lands on 4 (the floor of 4.5/4.75), not on 5.
    progress_set(&db.pool, user, series, 6.0)
        .await
        .expect("set progress");
    progress_mark_unread(&db.pool, user, series, 6.0)
        .await
        .expect("mark unread");
    assert_eq!(frontiers(&db, user, series).await, (4.0, None));
}

/// **TRACK-2.** Un-reading the part frontier itself falls back to the next part still ahead of the
/// whole frontier, not to nothing.
///
/// The bug this pins: `progress_mark_unread` special-cased "the number *is* the part frontier" and
/// cleared the frontier outright. A single scalar cannot say "`152.6` unread, `152.5` read" by
/// clearing itself — it says it by retreating — so with `152.1`..`152.6` read, un-reading `152.6`
/// reported the other five unread as well. One click, five chapters of progress gone, no undo, and
/// nothing in the response to suggest it had happened. The branch was also redundant: the general
/// retreat below it computes exactly the same answer when no earlier part exists.
///
/// The second half pins the bound `prev_part_below` retreats within. It is
/// `floor(c.number) > whole`, so the fallback cannot land on a part of the whole frontier's *own*
/// chapter — `3.5` is already covered by a frontier at `3`, and recording it as the part frontier
/// would be the one `floor(part) == whole` shape no other write site can produce.
#[tokio::test]
async fn un_reading_the_part_frontier_falls_back_to_the_previous_part_ahead() {
    let db = TestDb::spawn().await;
    let user = seed::user(&db, "reader").create().await;
    let provider = seed::provider(&db, "alpha").create().await;
    let series = a_series(&db, provider, "Berserk", CHAPTERS).await;

    progress_set(&db.pool, user, series, 3.0)
        .await
        .expect("set progress");
    progress_mark_read(&db.pool, user, series, 4.75)
        .await
        .expect("mark read");
    progress_mark_unread(&db.pool, user, series, 4.75)
        .await
        .expect("mark unread");
    assert_eq!(
        frontiers(&db, user, series).await,
        (3.0, Some(4.5)),
        "4.5 is still read and still ahead of the whole frontier"
    );

    progress_mark_unread(&db.pool, user, series, 4.5)
        .await
        .expect("mark unread");
    assert_eq!(
        frontiers(&db, user, series).await,
        (3.0, None),
        "3.5 is below the whole frontier, so it cannot become the part frontier"
    );
}

/// **TRACK-2, the other half.** Un-reading a part that was never read must not *advance* the part
/// frontier.
///
/// The retreat target comes from the catalogue, not from the frontier, so applying it
/// unconditionally is how un-reading one chapter marks a different one read: with nothing ahead of
/// the whole frontier read, un-reading `4.75` set the frontier to `4.5` — the highest part below it
/// — and `4.5` started reporting as read. The same shape when the frontier is merely below
/// `number`: it must stay where it is.
///
/// This is the direction a test is least likely to be written for, because "un-read" reads as a
/// verb that can only ever remove.
#[tokio::test]
async fn un_reading_a_part_that_was_never_read_does_not_advance_the_frontier() {
    let db = TestDb::spawn().await;
    let user = seed::user(&db, "reader").create().await;
    let provider = seed::provider(&db, "alpha").create().await;
    let series = a_series(&db, provider, "Berserk", CHAPTERS).await;

    // Nothing ahead of the whole frontier is read.
    progress_set(&db.pool, user, series, 3.0)
        .await
        .expect("set progress");
    progress_mark_unread(&db.pool, user, series, 4.75)
        .await
        .expect("mark unread");
    assert_eq!(
        frontiers(&db, user, series).await,
        (3.0, None),
        "un-reading 4.75 must not record 4.5 as read"
    );

    // The frontier is below `number`: already unread, so nothing moves.
    progress_mark_read(&db.pool, user, series, 4.5)
        .await
        .expect("mark read");
    progress_mark_unread(&db.pool, user, series, 4.75)
        .await
        .expect("mark unread");
    assert_eq!(frontiers(&db, user, series).await, (3.0, Some(4.5)));
}

/// **OPS-2.2d.** Excluding a series the user does not track must report that nothing was written.
///
/// `sync_excluded` is a column on `watchlist_entries`, so the `UPDATE` has nowhere to land until
/// the series is tracked. The function used to discard `rows_affected` and answer `Ok(())`, and
/// `PUT /v1/me/watchlist/{series_id}/sync` answered `{"ok": true}` on top of it — so a user who
/// opted a series out of external sync *before* adding it to their watchlist was told the opt-out
/// had been saved, nothing had been, and the next sync pushed their progress to the provider.
///
/// The second half is what makes this a regression test rather than a tautology: the same call
/// against a tracked series must return `true` **and** be visible to
/// [`is_sync_excluded`], which is the choke point every sync path actually consults. Asserting
/// only the boolean would pass against a function that returned `true` without writing.
#[tokio::test]
async fn excluding_an_untracked_series_reports_that_nothing_was_written() {
    let db = TestDb::spawn().await;
    let user = seed::user(&db, "reader").create().await;
    let provider = seed::provider(&db, "alpha").create().await;
    let series = a_series(&db, provider, "Berserk", CHAPTERS).await;

    assert!(
        !set_sync_excluded(&db.pool, user, series, true)
            .await
            .expect("exclude an untracked series"),
        "an untracked series has no row to carry the flag; this must not report success"
    );
    assert!(
        !is_sync_excluded(&db.pool, user, series, "anilist")
            .await
            .expect("read the exclusion"),
        "nothing was written, so the choke point must still say `included`"
    );

    watchlist_upsert(&db.pool, user, series, WatchStatus::Reading, true)
        .await
        .expect("track the series");

    assert!(
        set_sync_excluded(&db.pool, user, series, true)
            .await
            .expect("exclude a tracked series"),
        "the entry exists now, so the flag lands"
    );
    assert!(
        is_sync_excluded(&db.pool, user, series, "anilist")
            .await
            .expect("read the exclusion"),
        "and the sync choke point honours it"
    );

    assert!(
        set_sync_excluded(&db.pool, user, series, false)
            .await
            .expect("clear the exclusion"),
        "clearing is a write like any other and reports the same way"
    );
    assert!(
        !is_sync_excluded(&db.pool, user, series, "anilist")
            .await
            .expect("read the exclusion")
    );
}

// ---------------------------------------------------------------------------
// Watchlist free-text search
// ---------------------------------------------------------------------------

/// One unfiltered-but-for-`term` watchlist page.
///
/// Deliberately reads the whole [`WatchlistPage`], not just `items`: the search predicate is
/// written out three times — the page, the tab counts and the group aggregates — and asserting on
/// `items` alone would pass with two of the three still wrong.
async fn search(db: &TestDb, user: UserId, term: &str) -> WatchlistPage {
    watchlist_page(
        &db.pool,
        user,
        &WatchlistFilter {
            query: Some(term.to_owned()),
            ..WatchlistFilter::default()
        },
    )
    .await
    .expect("watchlist search")
}

/// Put every one of `series` on `user`'s watchlist.
async fn track_all(db: &TestDb, user: UserId, series: &[SeriesId]) {
    for id in series {
        watchlist_upsert(&db.pool, user, *id, WatchStatus::Reading, true)
            .await
            .expect("watchlist");
    }
}

/// Search reaches every column it advertises: the canonical title, an alternative title, a tag
/// name and an author name — case-insensitively, on a substring.
#[tokio::test]
async fn the_search_matches_canonical_titles_alt_titles_tags_and_authors() {
    let db = TestDb::spawn().await;
    let user = seed::user(&db, "searcher").create().await;
    let provider = seed::provider(&db, "alpha").create().await;

    let canonical = seed::series(&db, provider, "Berserk")
        .source_path("/s/berserk")
        .chapters(&[1.0])
        .create()
        .await;
    let alternate = seed::series(&db, provider, "Claymore")
        .source_path("/s/claymore")
        .alt_titles(&["Kureimoa"])
        .chapters(&[1.0])
        .create()
        .await;
    let tagged = seed::series(&db, provider, "Dorohedoro")
        .source_path("/s/dorohedoro")
        .tags(&["Seinen"])
        .chapters(&[1.0])
        .create()
        .await;
    let credited = seed::series(&db, provider, "Eden")
        .source_path("/s/eden")
        .authors(&["Kentaro Miura"])
        .chapters(&[1.0])
        .create()
        .await;
    track_all(&db, user, &[canonical, alternate, tagged, credited]).await;

    // Lower-cased and clipped short, so a branch that lost either property fails here.
    for (term, expected) in [
        ("erser", canonical),
        ("kureimo", alternate),
        ("seine", tagged),
        ("miur", credited),
    ] {
        let page = search(&db, user, term).await;
        assert_eq!(
            page.items.iter().map(|c| c.series_id).collect::<Vec<_>>(),
            vec![expected],
            "term={term:?}"
        );
        assert_eq!(page.total, 1, "group aggregates disagree for term={term:?}");
        assert_eq!(page.counts.all, 1, "tab counts disagree for term={term:?}");
    }
}

/// A term containing a LIKE metacharacter is matched literally.
///
/// The predicate used to be `strpos(lower(col), lower($n)) > 0`, which no index can serve, so it
/// moved to `col ILIKE $n`. That hands `%`, `_` and `\` — ordinary characters to whoever typed
/// them into the filter box — straight to the pattern matcher. Unescaped, `_` and `%` each widen
/// the search to the entire watchlist, and a term ending in `\` is a dangling escape that Postgres
/// rejects, turning a keystroke into a 500 from the filter box.
#[tokio::test]
async fn a_search_term_with_like_metacharacters_matches_literally() {
    let db = TestDb::spawn().await;
    let user = seed::user(&db, "escaper").create().await;
    let provider = seed::provider(&db, "alpha").create().await;

    let underscore = seed::series(&db, provider, "Under_score")
        .source_path("/s/underscore")
        .chapters(&[1.0])
        .create()
        .await;
    let percent = seed::series(&db, provider, "100% Orange")
        .source_path("/s/orange")
        .chapters(&[1.0])
        .create()
        .await;
    let backslash = seed::series(&db, provider, "Back\\slash")
        .source_path("/s/backslash")
        .chapters(&[1.0])
        .create()
        .await;
    let plain = seed::series(&db, provider, "Plain Title")
        .source_path("/s/plain")
        .chapters(&[1.0])
        .create()
        .await;
    track_all(&db, user, &[underscore, percent, backslash, plain]).await;

    for (term, expected) in [("_", underscore), ("%", percent), ("\\", backslash)] {
        let page = search(&db, user, term).await;
        assert_eq!(
            page.items.iter().map(|c| c.series_id).collect::<Vec<_>>(),
            vec![expected],
            "term={term:?} was treated as a wildcard"
        );
        assert_eq!(page.total, 1, "group aggregates disagree for term={term:?}");
        assert_eq!(page.counts.all, 1, "tab counts disagree for term={term:?}");
    }
}

/// A blank or whitespace-only query is no search, not a search for nothing — the same contract
/// `repo_browse`'s `a_blank_query_is_not_a_filter` pins for Discover.
#[tokio::test]
async fn a_blank_watchlist_query_is_not_a_filter() {
    let db = TestDb::spawn().await;
    let user = seed::user(&db, "idler").create().await;
    let provider = seed::provider(&db, "alpha").create().await;

    for title in ["Akira", "Berserk", "Claymore"] {
        let series = a_series(&db, provider, title, &[1.0]).await;
        watchlist_upsert(&db.pool, user, series, WatchStatus::Reading, true)
            .await
            .expect("watchlist");
    }

    for term in ["", "   "] {
        let page = search(&db, user, term).await;
        assert_eq!(page.items.len(), 3, "term={term:?}");
        assert_eq!(page.total, 3, "term={term:?}");
        assert_eq!(page.counts.all, 3, "term={term:?}");
    }
}

// ---------------------------------------------------------------------------
// The per-series source pin
// ---------------------------------------------------------------------------

/// The `series_sources` id of a series' only source.
async fn only_source(db: &TestDb, series_id: SeriesId) -> SeriesSourceId {
    let id: uuid::Uuid = sqlx::query_scalar("SELECT id FROM series_sources WHERE series_id = $1")
        .bind(series_id.as_uuid())
        .fetch_one(&db.pool)
        .await
        .expect("the seeded series has a source");
    SeriesSourceId::from_uuid(id)
}

/// A pin must be refused when the source belongs to a different series.
///
/// `series_sources` ids are global, and the foreign key only says "this row exists" — so without
/// the `EXISTS` that scopes the update to `$2`, a client could point one series' entry at another
/// series' source. Nothing downstream re-checks it: the pin is taken as the resolved answer, so
/// every `Open` on the screen would send the reader to a source for a different work. The failure
/// is silent, which is why it is pinned here rather than left to the handler.
#[tokio::test]
async fn a_pin_cannot_point_at_another_series_source() {
    let db = TestDb::spawn().await;
    let user = seed::user(&db, "pinner")
        .email("pinner@example.test")
        .create()
        .await;
    let provider = seed::provider(&db, "pin-provider").create().await;
    let mine = a_series(&db, provider, "Mine", &[1.0]).await;
    let theirs = a_series(&db, provider, "Theirs", &[1.0]).await;
    let foreign = only_source(&db, theirs).await;

    watchlist_upsert(&db.pool, user, mine, WatchStatus::Reading, true)
        .await
        .expect("track the series");

    let outcome = watchlist_set_pinned_source(&db.pool, user, mine, Some(foreign))
        .await
        .expect("the write itself succeeds");
    assert_eq!(
        outcome,
        PinOutcome::ForeignSource,
        "a source carrying a different series is refused, not stored"
    );

    let own = only_source(&db, mine).await;
    assert_eq!(
        watchlist_set_pinned_source(&db.pool, user, mine, Some(own))
            .await
            .expect("pin the series' own source"),
        PinOutcome::Written,
    );
}

/// Pinning writes to the watchlist entry, so an untracked series has to be distinguishable from
/// a rejected source — the API answers `404` for one and `400` for the other, and it can only do
/// that if this layer tells them apart.
#[tokio::test]
async fn pinning_an_untracked_series_is_reported_as_untracked() {
    let db = TestDb::spawn().await;
    let user = seed::user(&db, "untracked-pinner")
        .email("untracked-pinner@example.test")
        .create()
        .await;
    let provider = seed::provider(&db, "untracked-provider").create().await;
    let series = a_series(&db, provider, "Not tracked", &[1.0]).await;
    let source = only_source(&db, series).await;

    assert_eq!(
        watchlist_set_pinned_source(&db.pool, user, series, Some(source))
            .await
            .expect("the write itself succeeds"),
        PinOutcome::NotTracked,
    );
}

/// A pin whose source is retired — by a merge, or a provider dropping the entry — must fall back
/// to the reader's global order rather than leaving the entry pointing at nothing. The schema
/// does it with `ON DELETE SET NULL`; this is what stops a later migration from "tidying" that
/// into a cascade, which would delete the whole watchlist entry instead.
#[tokio::test]
async fn retiring_the_pinned_source_clears_the_pin_and_keeps_the_entry() {
    let db = TestDb::spawn().await;
    let user = seed::user(&db, "merge-pinner")
        .email("merge-pinner@example.test")
        .create()
        .await;
    let provider = seed::provider(&db, "merge-provider").create().await;
    let series = a_series(&db, provider, "Merged away", &[1.0]).await;
    let source = only_source(&db, series).await;

    watchlist_upsert(&db.pool, user, series, WatchStatus::Reading, true)
        .await
        .expect("track the series");
    watchlist_set_pinned_source(&db.pool, user, series, Some(source))
        .await
        .expect("pin the source");

    sqlx::query("DELETE FROM series_sources WHERE id = $1")
        .bind(source.as_uuid())
        .execute(&db.pool)
        .await
        .expect("retire the source");

    let card = watchlist_card(&db.pool, user, series)
        .await
        .expect("read the card")
        .expect("the entry survives its pinned source");
    assert_eq!(
        card.pinned_source_id, None,
        "the pin is cleared, not dangling"
    );
}

/// The continue-reading badge for one series, which is the surface the unread predicate is most
/// directly visible on.
async fn unread_now(db: &TestDb, user: UserId, series: SeriesId) -> i64 {
    continue_reading(&db.pool, user)
        .await
        .expect("continue")
        .iter()
        .find(|c| c.series_id == series)
        .map_or(0, |c| c.unread)
}

/// A paid early-access chapter must not be counted as unread until the reader can actually open
/// it — and must start counting the moment they can, by either route.
///
/// The bug this pins is the one the whole early-access model exists to prevent: before it, a
/// chapter a provider had published behind a paywall was ingested as an ordinary chapter, so it
/// inflated every unread badge, produced a "continue reading" card pointing at a page that
/// answers with a paywall, and fired a new-chapter notification for something unreadable. The
/// opposite mistake is just as bad and is covered here too — dropping the row at ingest loses
/// the chapter that has to exist when the timer expires, and re-discovering it later re-dates it.
///
/// All three surfaces are asserted because the predicate is spelled out eight times; a copy that
/// misses the access clause is exactly the drift this file was written to catch.
#[tokio::test]
async fn an_early_access_chapter_counts_only_once_the_reader_can_read_it() {
    let db = TestDb::spawn().await;
    let user = seed::user(&db, "reader").create().await;
    let provider = seed::provider(&db, "paid").create().await;
    let series = a_series(&db, provider, "Paywalled", &[1.0, 2.0, 3.0]).await;
    watchlist_upsert(&db.pool, user, series, WatchStatus::Reading, true)
        .await
        .expect("watchlist");

    assert_eq!(
        unread_now(&db, user, series).await,
        3,
        "premise: three free chapters"
    );

    // Chapter 3 goes behind the paywall, unlocking in a week.
    sqlx::query(
        "UPDATE chapters SET access = 'early_access', unlocks_at = now() + interval '7 days' \
         WHERE number_milli = 30000",
    )
    .execute(&db.pool)
    .await
    .expect("lock chapter 3");
    assert_eq!(
        unread_now(&db, user, series).await,
        2,
        "a locked chapter must not be counted as unread"
    );
    assert!(
        !feed(&db.pool, user, 100)
            .await
            .expect("feed")
            .iter()
            .any(|item| (item.chapter_number - 3.0).abs() < f64::EPSILON),
        "the release feed must not offer a chapter that answers with a paywall"
    );

    // Route one: the reader pays, and opts this provider in.
    tankovault_db::repo::users::set_early_access_providers(&db.pool, user, &[provider])
        .await
        .expect("opt in");
    assert_eq!(
        unread_now(&db, user, series).await,
        3,
        "an opted-in reader counts the chapters they have paid for"
    );

    // The opt-in is per provider, so another provider's paywall stays shut for the same reader.
    tankovault_db::repo::users::set_early_access_providers(&db.pool, user, &[])
        .await
        .expect("opt out");
    assert_eq!(
        unread_now(&db, user, series).await,
        2,
        "opting out closes it again"
    );

    // Route two: the timer expires. No rescan is needed — the stored unlock time is what the
    // predicate compares against, so the chapter opens on its own.
    sqlx::query(
        "UPDATE chapters SET unlocks_at = now() - interval '1 minute' WHERE number_milli = 30000",
    )
    .execute(&db.pool)
    .await
    .expect("expire the timer");
    assert_eq!(
        unread_now(&db, user, series).await,
        3,
        "a chapter whose unlock time has passed counts without a rescan"
    );

    // A locked chapter with no announced date must stay locked rather than defaulting to open.
    sqlx::query("UPDATE chapters SET unlocks_at = NULL WHERE number_milli = 30000")
        .execute(&db.pool)
        .await
        .expect("clear the date");
    assert_eq!(
        unread_now(&db, user, series).await,
        2,
        "no announced unlock time must read as still locked, never as already unlocked"
    );
}

/// The lookup the notifier's fan-out narrows a paywalled chapter's recipients with.
///
/// It is the same reader-level question the unread predicate answers inline, asked in the one
/// place that cannot ask it in SQL: the notifier already holds the claimed watcher list in
/// memory. It is separately pinned because the notifier consumes it as a *whitelist* — an empty
/// result means "announce to nobody", and a version that returned every user on no match, or
/// ignored the provider, would silently restore the bug it exists to close.
#[tokio::test]
async fn the_early_access_opt_in_lookup_is_scoped_to_one_provider() {
    let db = TestDb::spawn().await;
    let paid = seed::user(&db, "paid").create().await;
    let unpaid = seed::user(&db, "unpaid").create().await;
    let provider = seed::provider(&db, "paywalled").create().await;
    let other = seed::provider(&db, "elsewhere").create().await;

    tankovault_db::repo::users::set_early_access_providers(&db.pool, paid, &[provider])
        .await
        .expect("opt in");

    let opted = early_access_opted_in(&db.pool, &[paid, unpaid], provider)
        .await
        .expect("lookup");
    assert_eq!(opted, vec![paid], "only the reader who opted in");

    // Paying one scanlator says nothing about any other.
    assert!(
        early_access_opted_in(&db.pool, &[paid, unpaid], other)
            .await
            .expect("lookup")
            .is_empty(),
        "the opt-in must not carry across providers"
    );

    // Never widened to "everyone" by an empty candidate set.
    assert!(
        early_access_opted_in(&db.pool, &[], provider)
            .await
            .expect("lookup")
            .is_empty()
    );
}

/// Every figure a watchlist card shows must describe the chapters this reader can actually open.
///
/// The unread count was gated by migration `0047`; the card's other numbers were not, so a
/// paywalled chapter still raised `latest_chapter_number` and `total_chapters`, and its
/// `discovered_at` still set `latest_chapter_at` — which is what the `released` sort orders by,
/// what the "updated since" filter compares, and what buckets a series into "today". The card
/// therefore announced a release the reader could not open, at the top of their list, next to an
/// unread count of zero.
#[tokio::test]
async fn a_watchlist_card_counts_only_chapters_the_reader_can_open() {
    let db = TestDb::spawn().await;
    let user = seed::user(&db, "reader").create().await;
    let provider = seed::provider(&db, "paywalled").create().await;
    let series = a_series(&db, provider, "Gated", &[1.0, 2.0, 3.0]).await;
    watchlist_upsert(&db.pool, user, series, WatchStatus::Reading, true)
        .await
        .expect("watchlist");

    // Chapters 1 and 2 are a week old; 3 is today, so "latest activity" is unambiguous.
    sqlx::query(
        "UPDATE chapters SET discovered_at = now() - interval '7 days' WHERE number_milli < 30000",
    )
    .execute(&db.pool)
    .await
    .expect("age the free chapters");

    let before = watchlist_card(&db.pool, user, series)
        .await
        .expect("card")
        .expect("entry");
    assert_eq!(before.total_chapters, 3, "premise: three free chapters");
    assert_eq!(before.latest_chapter_number, Some(3.0));

    // Chapter 3 goes behind the paywall, and is the newest thing the series has.
    sqlx::query(
        "UPDATE chapters SET access = 'early_access', unlocks_at = now() + interval '7 days', \
                discovered_at = now() WHERE number_milli = 30000",
    )
    .execute(&db.pool)
    .await
    .expect("lock chapter 3");

    let locked = watchlist_card(&db.pool, user, series)
        .await
        .expect("card")
        .expect("entry");
    assert_eq!(
        locked.latest_chapter_number,
        Some(2.0),
        "the card must not advertise a chapter that answers with a paywall"
    );
    assert_eq!(locked.total_chapters, 2, "nor count it toward the total");
    let a_day_ago = time::OffsetDateTime::now_utc() - time::Duration::days(1);
    assert!(
        locked.latest_chapter_at.is_some_and(|at| at < a_day_ago),
        "a locked release must not register as this series' latest activity: {:?}",
        locked.latest_chapter_at
    );

    // Paying for it restores every figure at once — one predicate, one opt-in.
    tankovault_db::repo::users::set_early_access_providers(&db.pool, user, &[provider])
        .await
        .expect("opt in");
    let paid = watchlist_card(&db.pool, user, series)
        .await
        .expect("card")
        .expect("entry");
    assert_eq!(paid.latest_chapter_number, Some(3.0));
    assert_eq!(paid.total_chapters, 3);
}

/// "Mark group read" must not swallow a chapter the reader cannot open.
///
/// The frontier only ever moves forward, so a locked chapter folded into it is gone: when its
/// timer expires it is already behind the frontier and never appears as unread. The reader loses
/// exactly the chapter they were waiting for, and no rescan can bring it back.
#[tokio::test]
async fn marking_a_group_read_stops_at_the_last_chapter_the_reader_can_open() {
    let db = TestDb::spawn().await;
    let user = seed::user(&db, "reader").create().await;
    let provider = seed::provider(&db, "paywalled").create().await;
    let series = a_series(&db, provider, "Gated", &[1.0, 2.0, 3.0]).await;
    watchlist_upsert(&db.pool, user, series, WatchStatus::Reading, true)
        .await
        .expect("watchlist");
    sqlx::query(
        "UPDATE chapters SET access = 'early_access', unlocks_at = now() + interval '7 days' \
         WHERE number_milli = 30000",
    )
    .execute(&db.pool)
    .await
    .expect("lock chapter 3");

    tankovault_db::repo::tracking::progress_bulk_mark_all_read(&db.pool, user, &[series.as_uuid()])
        .await
        .expect("mark group read");

    let (whole, _) = frontiers(&db, user, series).await;
    assert!(
        (whole - 2.0).abs() < f64::EPSILON,
        "the frontier stops at 2, not 3: {whole}"
    );

    // And when the timer expires, the chapter is there waiting rather than already consumed.
    sqlx::query(
        "UPDATE chapters SET unlocks_at = now() - interval '1 minute' WHERE number_milli = 30000",
    )
    .execute(&db.pool)
    .await
    .expect("expire the timer");
    assert_eq!(
        unread_now(&db, user, series).await,
        1,
        "the chapter the reader waited for is unread, not swallowed"
    );
}

// ---------------------------------------------------------------------------
// Reading a chapter means following the series
// ---------------------------------------------------------------------------

/// A series the reader has never tracked is on the watchlist after `watchlist_track_if_absent`.
///
/// The bug: marking a chapter read wrote `read_progress` and nothing else, so a series opened
/// from Discover or Search stayed off the watchlist. Progress was recorded against a series the
/// reader could not see anywhere in Library — no unread badge, no notifications, no continue
/// card — and the only way to find it again was to search for it a second time.
#[tokio::test]
async fn tracking_an_untracked_series_adds_it_at_the_defaults_a_manual_add_uses() {
    let db = TestDb::spawn().await;
    let user = seed::user(&db, "reader").create().await;
    let provider = seed::provider(&db, "alpha").create().await;
    let series = a_series(&db, provider, "Berserk", CHAPTERS).await;

    watchlist_track_if_absent(&db.pool, user, series)
        .await
        .expect("track");

    let entries = watchlist_list(&db.pool, user).await.expect("list");
    let [entry] = entries.as_slice() else {
        panic!("expected exactly one entry, got {}", entries.len())
    };
    assert_eq!(entry.series_id, series);
    assert_eq!(entry.status, WatchStatus::Reading);
    assert!(entry.notify, "notify defaults on, as a manual add does");
}

/// An entry that already exists survives untouched — status and notify flag included.
///
/// The `DO NOTHING` half is the whole reason this is not `watchlist_upsert` with default
/// arguments. A reader who set `dropped` and muted the bell, then reads one more chapter out of
/// curiosity, would otherwise find the series back at `reading` with notifications re-armed, and
/// nothing in the UI would explain why.
#[tokio::test]
async fn tracking_a_series_already_on_the_watchlist_changes_nothing() {
    let db = TestDb::spawn().await;
    let user = seed::user(&db, "reader").create().await;
    let provider = seed::provider(&db, "alpha").create().await;
    let series = a_series(&db, provider, "Berserk", CHAPTERS).await;
    watchlist_upsert(&db.pool, user, series, WatchStatus::Dropped, false)
        .await
        .expect("watchlist");

    watchlist_track_if_absent(&db.pool, user, series)
        .await
        .expect("track");

    let entries = watchlist_list(&db.pool, user).await.expect("list");
    let [entry] = entries.as_slice() else {
        panic!("expected exactly one entry, got {}", entries.len())
    };
    assert_eq!(entry.status, WatchStatus::Dropped);
    assert!(!entry.notify, "the muted bell stays muted");
}
