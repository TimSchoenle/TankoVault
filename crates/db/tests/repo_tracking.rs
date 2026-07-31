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

use tankovault_db::repo::tracking::{
    ReadProgress, continue_reading, feed, is_sync_excluded, me_stats, progress_get_full,
    progress_mark_read, progress_mark_unread, progress_set, set_sync_excluded, watchers_for_series,
    watchlist_detailed, watchlist_upsert,
};
use tankovault_domain::{ProviderId, SeriesId, UserId, WatchStatus};
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

        // 3. The watchlist badge.
        let cards = watchlist_detailed(&db.pool, user).await.expect("watchlist");
        let card = cards
            .iter()
            .find(|c| c.series_id == series)
            .expect("the watched series");
        assert_eq!(
            card.unread, expected_count,
            "watchlist_detailed unread count ({})",
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
