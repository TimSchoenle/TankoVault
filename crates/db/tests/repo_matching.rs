//! Series canonicalisation: the candidate queries and what the repository *persists* once the
//! policy has decided (`crates/db/src/repo/matching.rs`, TEST F-05).
//!
//! # The split, and what is left here to test
//!
//! ARCH-16 step 3 moved the *decision* out of this crate: `crates/db` reads trigram candidates,
//! asks a [`Canonicaliser`](tankovault_domain::matching::Canonicaliser), and performs the answer.
//! The decision has four unit tests of its own in `crates/config/src/matching.rs`. What has none
//! — and cannot have any without a database, because the whole of it is SQL — is the half that
//! remains:
//!
//! - **What the scorer is given.** `find_candidates` decides which existing series are even
//!   *considered*, and supplies the similarity, the medium, the year, the tags and the authors that
//!   every bonus in `tankovault_matcher::score` is computed from. A dropped `array_agg` subquery
//!   compiles, returns an empty array, and silently turns off the tag and author signals — so the
//!   matcher gets quieter and more duplicate series appear, with nothing to point at.
//! - **What the repository writes.** The ambiguous band is the only outcome with a side effect
//!   beyond the series row: a `merge_candidates` row is the *only* record that two series might be
//!   one, and if it is not written the pair is silently split forever.
//! - **`merge_series`.** Eleven statements that move one series' entire footprint onto another and
//!   then delete it. It is irreversible, it touches user data (`watchlist_entries`,
//!   `read_progress`), and a dropped statement loses whatever that table held.
//!
//! # Where the thresholds come from in these tests
//!
//! `MatchingConfig` *is* the policy object, so the tests configure it rather than substituting a
//! stub — which is also what makes the ambiguous band reachable without depending on the exact
//! trigram similarity Postgres computes for a chosen pair of titles. `high` and `low` are set to
//! bracket whatever score the real scorer produces, so the test asserts the *outcome* the band
//! produces rather than the score, and does not become brittle to a `pg_trgm` version change.
//!
//! Opt-in: gated behind the `integration` feature because it requires Docker.
#![cfg(feature = "integration")]

use tankovault_config::MatchingConfig;
use tankovault_db::DbError;
use tankovault_db::repo::catalog::{
    ChapterUpsert, ScannedSeries, SeriesUpsert, ingest_series, list_series_titles,
    register_source_stub,
};
use tankovault_db::repo::matching::{
    MergeCandidateView, dismiss_merge_candidate, find_candidates, find_candidates_multi,
    list_open_merge_candidates, merge_series,
};
use tankovault_db::repo::sync;
use tankovault_db::repo::tracking::{
    ReadProgress, progress_get_full, progress_mark_read, progress_set, watchlist_list,
    watchlist_upsert,
};
use tankovault_domain::{
    ContentType, ProviderId, SeriesId, SeriesStatus, UserId, WatchStatus, normalize_title,
};
use tankovault_test_support::{TestDb, seed};

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

/// Every candidate row this suite asks about, so the assertions can name a title rather than
/// carrying a similarity number that a `pg_trgm` upgrade could move.
struct Seed {
    title: &'static str,
    content_type: ContentType,
    release_year: Option<i32>,
    tags: &'static [&'static str],
    authors: &'static [&'static str],
    alt_titles: &'static [&'static str],
}

async fn ingest(db: &TestDb, provider_id: ProviderId, seed: &Seed, chapters: &[f64]) -> SeriesId {
    ingest_series(
        &db.pool,
        &ScannedSeries {
            provider_id,
            source_path: format!(
                "/s/{}-{}",
                normalize_title(seed.title).replace(' ', "-"),
                provider_id.as_uuid().simple()
            ),
            provider_title: Some(seed.title.to_owned()),
            meta: SeriesUpsert {
                canonical_title: seed.title.to_owned(),
                normalized_title: normalize_title(seed.title),
                description: None,
                cover_url: None,
                content_type: seed.content_type,
                status: SeriesStatus::Ongoing,
                release_year: seed.release_year,
            },
            alt_titles: seed
                .alt_titles
                .iter()
                .map(|t| ((*t).to_owned(), normalize_title(t)))
                .collect(),
            tags: seed.tags.iter().map(|t| (*t).to_owned()).collect(),
            authors: seed.authors.iter().map(|a| (*a).to_owned()).collect(),
            chapters: chapters
                .iter()
                .map(|n| ChapterUpsert {
                    number: *n,
                    volume: None,
                    title: None,
                    path: format!("/c/{n}"),
                    published_at: None,
                })
                .collect(),
            content_hash: vec![1],
        },
        &MatchingConfig::default(),
    )
    .await
    .expect("ingest series")
    .series_id
}

/// A policy that puts every trigram candidate in the **ambiguous** band.
///
/// `high` above 1.0 is unreachable (the scorer clamps to `[0,1]`) and `low` at 0.0 admits
/// everything, so any candidate the `%` operator returns is ambiguous. That makes the band
/// reachable without asserting a similarity value, which is the part of this a `pg_trgm` change
/// could move.
fn always_ambiguous() -> MatchingConfig {
    MatchingConfig {
        high: 1.01,
        low: 0.0,
        ..MatchingConfig::default()
    }
}

/// A user holding progress on **both** series of a merge pair, which is the only shape where the
/// `ON CONFLICT DO UPDATE` arm of the read-progress merge runs at all.
///
/// `part` is marked read on the *absorbed* series, so it is reached through the real
/// [`progress_mark_read`] rule rather than planted — the fixture must not be able to produce a pair
/// of frontiers that no write path can.
/// The two arguments are `(surviving series, its whole frontier)` and
/// `(absorbed series, its whole frontier, a part release read on it)` — grouped per series rather
/// than as six positional scalars, because adjacent same-typed arguments transpose silently and
/// getting the two sides the wrong way round is precisely what this test exists to detect.
async fn a_reader(
    db: &TestDb,
    name: &str,
    keep: (SeriesId, f64),
    drop: (SeriesId, f64, Option<f64>),
) -> UserId {
    let user = seed::user(db, name).create().await;
    progress_set(&db.pool, user, keep.0, keep.1)
        .await
        .expect("progress on the survivor");
    progress_set(&db.pool, user, drop.0, drop.1)
        .await
        .expect("progress on the absorbed series");
    if let Some(part) = drop.2 {
        progress_mark_read(&db.pool, user, drop.0, part)
            .await
            .expect("mark a part read");
    }
    user
}

/// How many merge candidates exist, resolved or not — `list_open_merge_candidates` filters, so a
/// count of everything is needed to tell "not written" from "written and resolved".
async fn merge_candidate_count(db: &TestDb) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM merge_candidates")
        .fetch_one(&db.pool)
        .await
        .expect("count merge candidates")
}

// ---------------------------------------------------------------------------
// find_candidates — what the scorer is given
// ---------------------------------------------------------------------------

/// A candidate is found through its **alternative** titles as well as its canonical one, and the
/// similarity reported is the best across all of them.
///
/// The `GREATEST(similarity(canonical), MAX(similarity(alt)))` is the difference between matching a
/// series by its romaji title and not matching it at all. Losing the `series_titles` half compiles,
/// still returns rows, and simply makes every non-English spelling of a work a new series — the
/// exact duplicate-series problem canonicalisation exists to prevent.
#[tokio::test]
async fn a_candidate_is_found_through_its_alternative_titles() {
    let db = TestDb::spawn().await;
    let provider = seed::provider(&db, "alpha").create().await;
    let solo = ingest(
        &db,
        provider,
        &Seed {
            title: "Solo Leveling",
            content_type: ContentType::Manhwa,
            release_year: Some(2018),
            tags: &["Action"],
            authors: &["Chugong"],
            alt_titles: &["Na Honjaman Level Up"],
        },
        &[1.0],
    )
    .await;

    // Nothing about the canonical title resembles the query; only the alternative does.
    let by_alt = find_candidates(&db.pool, &normalize_title("Na Honjaman Level Up"), 10)
        .await
        .expect("find candidates");
    let matched = by_alt
        .iter()
        .find(|c| c.series_id == solo)
        .expect("the alternative title must reach the series");
    assert!(
        matched.similarity > 0.9,
        "the similarity reported is the alternative title's, not the canonical title's: {}",
        matched.similarity
    );

    let by_canonical = find_candidates(&db.pool, &normalize_title("Solo Leveling"), 10)
        .await
        .expect("find candidates");
    assert!(by_canonical.iter().any(|c| c.series_id == solo));
}

/// Every candidate carries the medium, the year, the tags and the authors the scorer needs.
///
/// These four fields are not decoration: `tankovault_matcher::score` adds `+0.08` for an agreeing
/// medium, up to `+0.05` for tag overlap and `+0.1` for a shared author credit, and subtracts for
/// disagreement. All four arrive through correlated subqueries with `COALESCE(..., '{}')`, so
/// dropping one yields an empty set rather than an error and turns its signal permanently off —
/// silently making the matcher more conservative than it is configured to be. `Candidate` is also
/// the *only* candidate type now (ARCH-16), shared with `services/sync`, so a field lost here is
/// lost on both paths at once.
#[tokio::test]
async fn a_candidate_carries_every_signal_the_scorer_reads() {
    let db = TestDb::spawn().await;
    let provider = seed::provider(&db, "alpha").create().await;
    let berserk = ingest(
        &db,
        provider,
        &Seed {
            title: "Berserk",
            content_type: ContentType::Manga,
            release_year: Some(1989),
            tags: &["Action", "Dark Fantasy"],
            authors: &["Kentaro Miura"],
            alt_titles: &[],
        },
        &[1.0],
    )
    .await;
    // A series with no tags, no authors and no year, so an empty vector proves "none recorded"
    // rather than "the subquery is broken".
    let bare = ingest(
        &db,
        provider,
        &Seed {
            title: "Berserker",
            content_type: ContentType::Unknown,
            release_year: None,
            tags: &[],
            authors: &[],
            alt_titles: &[],
        },
        &[1.0],
    )
    .await;

    let found = find_candidates(&db.pool, &normalize_title("Berserk"), 10)
        .await
        .expect("find candidates");

    let rich = found
        .iter()
        .find(|c| c.series_id == berserk)
        .expect("the enriched series");
    assert_eq!(rich.content_type, ContentType::Manga);
    assert_eq!(rich.release_year, Some(1989));
    let mut tags = rich.tags.clone();
    tags.sort();
    assert_eq!(tags, vec!["Action", "Dark Fantasy"]);
    assert_eq!(rich.authors, vec!["Kentaro Miura"]);
    assert_eq!(rich.normalized_title, "berserk");

    let bare = found
        .iter()
        .find(|c| c.series_id == bare)
        .expect("the bare series");
    assert_eq!(bare.content_type, ContentType::Unknown);
    assert_eq!(bare.release_year, None);
    assert!(
        bare.tags.is_empty() && bare.authors.is_empty(),
        "absent tags/authors must arrive as an empty vector, never as a failed decode"
    );
}

/// Candidates come back best-first and the limit keeps the best, not an arbitrary subset.
///
/// The ordering is written as **`ORDER BY 5 DESC`** — a positional reference to the `sim`
/// expression. Inserting a column ahead of it re-sorts the whole result by something else
/// entirely (`release_year`, in the current shape) and `LIMIT` then discards the *best* candidates
/// instead of the worst, so the true match can fall out of the set the policy ever sees. Nothing
/// else notices: the query still compiles and still returns candidates. The same positional
/// hazard is pinned for the admin suggestion list in `repo_sync_admin.rs`.
#[tokio::test]
async fn candidates_are_ordered_best_first_and_the_limit_keeps_the_best() {
    let db = TestDb::spawn().await;
    let provider = seed::provider(&db, "alpha").create().await;
    // Deliberately descending in similarity to "solo leveling" and *ascending* in release year, so
    // an ordering that fell through to the year would produce the exact reverse.
    for (title, year) in [
        ("Solo Leveling", 2018),
        ("Solo Leveling Side Story", 2019),
        ("Solo Farming In The Tower", 2021),
    ] {
        ingest(
            &db,
            provider,
            &Seed {
                title,
                content_type: ContentType::Manhwa,
                release_year: Some(year),
                tags: &[],
                authors: &[],
                alt_titles: &[],
            },
            &[1.0],
        )
        .await;
    }

    let found = find_candidates(&db.pool, &normalize_title("Solo Leveling"), 10)
        .await
        .expect("find candidates");
    assert!(
        found.len() >= 2,
        "the corpus must produce several candidates"
    );
    for pair in found.windows(2) {
        assert!(
            pair[0].similarity >= pair[1].similarity,
            "candidates must be ordered by descending similarity, got {:?}",
            found.iter().map(|c| c.similarity).collect::<Vec<_>>()
        );
    }
    assert_eq!(found[0].normalized_title, "solo leveling");

    let capped = find_candidates(&db.pool, &normalize_title("Solo Leveling"), 1)
        .await
        .expect("find candidates");
    assert_eq!(capped.len(), 1);
    assert_eq!(
        capped[0].normalized_title, "solo leveling",
        "the limit must keep the best candidate, not the first row scanned"
    );
}

/// `find_candidates_multi` returns exactly what `find_candidates` would return per title, in the
/// caller's order, with a bucket for every requested title.
///
/// This is a differential test between the two forms, and PERF-13's claim is precisely that they
/// are interchangeable: the lateral join exists so `LIMIT` still applies **per title**, so a title
/// with many weak candidates cannot crowd out another title's strong one. If the `LIMIT` escaped
/// the lateral, or the similarity expression drifted between the two statements, an `AniList` entry
/// would attach on the single-title path and not on the batched one — which is the shape of bug
/// nobody finds, because both paths look like they work.
///
/// The empty bucket matters too: `services/sync` iterates the returned pairs, so a title dropped
/// for having no candidates would silently shorten the title family being scored.
#[tokio::test]
async fn the_batched_candidate_lookup_agrees_with_the_single_title_form() {
    let db = TestDb::spawn().await;
    let provider = seed::provider(&db, "alpha").create().await;
    for title in [
        "Solo Leveling",
        "Solo Leveling Side Story",
        "Berserk",
        "Berserker",
        "Vinland Saga",
    ] {
        ingest(
            &db,
            provider,
            &Seed {
                title,
                content_type: ContentType::Manga,
                release_year: None,
                tags: &["Action"],
                authors: &["Someone"],
                alt_titles: &[],
            },
            &[1.0],
        )
        .await;
    }

    let queries: Vec<String> = ["Solo Leveling", "Berserk", "Nothing Like This At All"]
        .iter()
        .map(|t| normalize_title(t))
        .collect();

    for limit in [1, 10] {
        let batched = find_candidates_multi(&db.pool, &queries, limit)
            .await
            .expect("batched lookup");
        assert_eq!(
            batched.iter().map(|(t, _)| t.clone()).collect::<Vec<_>>(),
            queries,
            "every requested title keeps its bucket, in the caller's order (limit {limit})"
        );

        for (title, candidates) in &batched {
            let single = find_candidates(&db.pool, title, limit)
                .await
                .expect("single lookup");
            assert_eq!(
                candidates
                    .iter()
                    .map(|c| (c.series_id, c.similarity))
                    .collect::<Vec<_>>(),
                single
                    .iter()
                    .map(|c| (c.series_id, c.similarity))
                    .collect::<Vec<_>>(),
                "the two forms disagree for {title:?} at limit {limit}"
            );
        }

        let (_, none) = batched
            .iter()
            .find(|(t, _)| t == "nothing like this at all")
            .expect("the no-candidate title keeps its bucket");
        assert!(none.is_empty());
    }

    assert!(
        find_candidates_multi(&db.pool, &[], 10)
            .await
            .expect("empty batch")
            .is_empty(),
        "an empty title list must not reach the database"
    );
}

/// A repeated query title collapses into one bucket — pinned as written, because the only caller
/// deduplicates first.
///
/// `UNNEST` scores a duplicated title twice and the bucketing takes the first match, so both sets
/// of rows land in the first bucket and the second is left empty — which also means the first
/// bucket can hold `2 × limit` candidates, breaking the per-title `LIMIT` the lateral exists for.
/// `services/sync`'s `series_for_entry` normalises and **deduplicates** the title family before
/// calling (`AniList` routinely repeats romaji and english), so this is unreachable today. It is
/// asserted rather than left implicit so that a second caller cannot discover it by accident.
#[tokio::test]
async fn a_repeated_query_title_collapses_into_one_bucket() {
    let db = TestDb::spawn().await;
    let provider = seed::provider(&db, "alpha").create().await;
    ingest(
        &db,
        provider,
        &Seed {
            title: "Berserk",
            content_type: ContentType::Manga,
            release_year: None,
            tags: &[],
            authors: &[],
            alt_titles: &[],
        },
        &[1.0],
    )
    .await;

    let repeated = vec!["berserk".to_owned(), "berserk".to_owned()];
    let batched = find_candidates_multi(&db.pool, &repeated, 10)
        .await
        .expect("batched lookup");
    assert_eq!(batched.len(), 2, "one bucket per requested title, still");
    assert_eq!(
        batched[0].1.len(),
        2,
        "the duplicate's rows land in the first bucket"
    );
    assert!(
        batched[1].1.is_empty(),
        "callers must deduplicate; the second bucket is empty, not a copy"
    );
}

// ---------------------------------------------------------------------------
// resolve_canonical_series — what the repository persists
// ---------------------------------------------------------------------------

/// A high-confidence match attaches the new source to the existing series and writes **no** merge
/// candidate.
///
/// The negative half is the one worth having: if the `Attach` arm ever recorded a candidate too,
/// the operator review queue would fill with every ordinary re-scan and the genuine ambiguities
/// would be unfindable in it.
#[tokio::test]
async fn a_confident_match_attaches_and_queues_nothing() {
    let db = TestDb::spawn().await;
    let alpha = seed::provider(&db, "alpha").create().await;
    let beta = seed::provider(&db, "beta").create().await;
    let existing = ingest(
        &db,
        alpha,
        &Seed {
            title: "Berserk",
            content_type: ContentType::Manga,
            release_year: Some(1989),
            tags: &[],
            authors: &[],
            alt_titles: &[],
        },
        &[1.0],
    )
    .await;

    // A second provider listing the same work, spelled differently but normalising the same way.
    register_source_stub(
        &db.pool,
        beta,
        "/manga/berserk",
        "BERSERK",
        &MatchingConfig::default(),
    )
    .await
    .expect("register stub");

    let series_count: i64 = sqlx::query_scalar("SELECT count(*) FROM series")
        .fetch_one(&db.pool)
        .await
        .expect("count series");
    assert_eq!(series_count, 1, "the two spellings are one series");
    let source_series: Vec<uuid::Uuid> =
        sqlx::query_scalar("SELECT series_id FROM series_sources ORDER BY source_path")
            .fetch_all(&db.pool)
            .await
            .expect("read sources");
    assert!(source_series.iter().all(|id| *id == existing.as_uuid()));
    assert_eq!(merge_candidate_count(&db).await, 0);
}

/// The ambiguous band creates a **new** series *and* records the merge candidate — both, in one
/// transaction.
///
/// This is the only outcome with a side effect beyond the series row, and it is the whole point of
/// having a middle band: the repository cannot decide, so it keeps the data separate and hands the
/// judgement to an operator. Losing the `record_merge_candidate` call leaves the split silently
/// permanent — two series, no queue entry, and nothing anywhere that says they might be one. The
/// series row is still created, so no error surfaces and no test that only counts series notices.
///
/// The recorded `score` and `reason` are asserted because they are what the console renders for the
/// operator to judge by; a candidate with no score is a coin flip.
#[tokio::test]
async fn the_ambiguous_band_creates_a_series_and_queues_the_pair() {
    let db = TestDb::spawn().await;
    let alpha = seed::provider(&db, "alpha").create().await;
    let beta = seed::provider(&db, "beta").create().await;
    let existing = ingest(
        &db,
        alpha,
        &Seed {
            title: "Berserk",
            content_type: ContentType::Manga,
            release_year: Some(1989),
            tags: &[],
            authors: &[],
            alt_titles: &[],
        },
        &[1.0],
    )
    .await;

    register_source_stub(
        &db.pool,
        beta,
        "/manga/berserk-2",
        "Berserk",
        &always_ambiguous(),
    )
    .await
    .expect("register stub");

    let series_count: i64 = sqlx::query_scalar("SELECT count(*) FROM series")
        .fetch_one(&db.pool)
        .await
        .expect("count series");
    assert_eq!(series_count, 2, "an ambiguous match must not attach");

    let queued = list_open_merge_candidates(&db.pool, 10)
        .await
        .expect("list merge candidates");
    assert_eq!(queued.len(), 1);
    let candidate = &queued[0];
    assert_ne!(
        candidate.series_id, existing,
        "series_id is the *new* series, candidate_id the one it might duplicate"
    );
    assert_eq!(candidate.candidate_id, existing);
    assert_eq!(candidate.series_title, "Berserk");
    assert_eq!(candidate.candidate_title, "Berserk");
    assert_eq!(candidate.reason.as_deref(), Some("ambiguous title match"));
    assert!(
        candidate.score > 0.0,
        "the operator judges by the score; {} is not a judgement",
        candidate.score
    );
}

/// Nothing similar enough means a new series and no queue entry.
///
/// The `Create` arm is the common case — every first source of a work takes it — so its cost has to
/// be zero. A merge candidate written here would make the queue grow with the catalogue.
#[tokio::test]
async fn an_unmatched_title_creates_a_series_and_queues_nothing() {
    let db = TestDb::spawn().await;
    let alpha = seed::provider(&db, "alpha").create().await;
    ingest(
        &db,
        alpha,
        &Seed {
            title: "Berserk",
            content_type: ContentType::Manga,
            release_year: None,
            tags: &[],
            authors: &[],
            alt_titles: &[],
        },
        &[1.0],
    )
    .await;

    register_source_stub(
        &db.pool,
        alpha,
        "/manga/vinland-saga",
        "Vinland Saga",
        &MatchingConfig::default(),
    )
    .await
    .expect("register stub");

    let series_count: i64 = sqlx::query_scalar("SELECT count(*) FROM series")
        .fetch_one(&db.pool)
        .await
        .expect("count series");
    assert_eq!(series_count, 2);
    assert_eq!(merge_candidate_count(&db).await, 0);
}

// ---------------------------------------------------------------------------
// The operator review queue
// ---------------------------------------------------------------------------

/// The queue lists only unresolved candidates, newest first, with both titles resolved.
///
/// Three separate things, each of which fails quietly. `WHERE NOT mc.resolved` is the difference
/// between a work queue and a log — without it an operator re-decides everything they have already
/// decided. Both `JOIN series` legs supply the titles the console renders; an inner join is correct
/// here (a candidate referencing a deleted series is meaningless) but means a dropped join makes
/// rows *vanish* rather than error. And the ordering is what makes the queue drainable.
#[tokio::test]
async fn the_merge_queue_lists_only_unresolved_candidates_newest_first() {
    let db = TestDb::spawn().await;
    let alpha = seed::provider(&db, "alpha").create().await;
    let beta = seed::provider(&db, "beta").create().await;
    let gamma = seed::provider(&db, "gamma").create().await;
    ingest(
        &db,
        alpha,
        &Seed {
            title: "Berserk",
            content_type: ContentType::Manga,
            release_year: None,
            tags: &[],
            authors: &[],
            alt_titles: &[],
        },
        &[1.0],
    )
    .await;

    // Two ambiguous registrations against it, in a known order.
    for (provider, path) in [(beta, "/b/berserk"), (gamma, "/g/berserk")] {
        register_source_stub(&db.pool, provider, path, "Berserk", &always_ambiguous())
            .await
            .expect("register stub");
    }
    sqlx::query(
        "UPDATE merge_candidates SET created_at = now() - (row_number * interval '1 day') \
         FROM (SELECT id, row_number() OVER (ORDER BY id) AS row_number FROM merge_candidates) o \
         WHERE merge_candidates.id = o.id",
    )
    .execute(&db.pool)
    .await
    .expect("spread the creation instants");

    let queued = list_open_merge_candidates(&db.pool, 10)
        .await
        .expect("list");
    assert_eq!(queued.len(), 2);
    assert!(
        queued[0].created_at > queued[1].created_at,
        "newest first, or an operator drains the queue from the wrong end"
    );

    let limited = list_open_merge_candidates(&db.pool, 1).await.expect("list");
    assert_eq!(limited.len(), 1);
    assert_eq!(limited[0].id, queued[0].id);

    // Resolving one takes it out of the queue and leaves the other.
    let actor = seed::user(&db, "operator").create().await;
    assert!(
        dismiss_merge_candidate(&db.pool, queued[0].id, Some(actor))
            .await
            .expect("dismiss"),
        "dismissing an open candidate reports that it did something"
    );
    let remaining: Vec<_> = list_open_merge_candidates(&db.pool, 10)
        .await
        .expect("list")
        .iter()
        .map(|c: &MergeCandidateView| c.id)
        .collect();
    assert_eq!(remaining, vec![queued[1].id]);
    assert_eq!(
        merge_candidate_count(&db).await,
        2,
        "resolving marks the row, it does not delete it — the decision is the audit trail"
    );
}

/// Dismissing a candidate is single-use, and the second attempt says so.
///
/// `AND NOT resolved` is what makes the boolean meaningful: without it the second call still
/// reports success and overwrites `resolved_by`/`resolved_at`, so the record says the last operator
/// to click made the decision rather than the one who actually did. The handler maps `false` to a
/// `404`, which is also how a double-submitted console form is distinguished from a live one.
#[tokio::test]
async fn dismissing_a_merge_candidate_is_single_use() {
    let db = TestDb::spawn().await;
    let alpha = seed::provider(&db, "alpha").create().await;
    let beta = seed::provider(&db, "beta").create().await;
    ingest(
        &db,
        alpha,
        &Seed {
            title: "Berserk",
            content_type: ContentType::Manga,
            release_year: None,
            tags: &[],
            authors: &[],
            alt_titles: &[],
        },
        &[1.0],
    )
    .await;
    register_source_stub(&db.pool, beta, "/b/berserk", "Berserk", &always_ambiguous())
        .await
        .expect("register stub");
    let candidate = list_open_merge_candidates(&db.pool, 10)
        .await
        .expect("list")[0]
        .id;

    let first = seed::user(&db, "first").create().await;
    let second = seed::user(&db, "second").create().await;
    assert!(
        dismiss_merge_candidate(&db.pool, candidate, Some(first))
            .await
            .expect("dismiss")
    );
    assert!(
        !dismiss_merge_candidate(&db.pool, candidate, Some(second))
            .await
            .expect("dismiss again"),
        "a resolved candidate cannot be resolved again"
    );

    let resolved_by: Option<uuid::Uuid> =
        sqlx::query_scalar("SELECT resolved_by FROM merge_candidates WHERE id = $1")
            .bind(candidate)
            .fetch_one(&db.pool)
            .await
            .expect("read resolved_by");
    assert_eq!(
        resolved_by,
        Some(first.as_uuid()),
        "the operator who decided must stay recorded"
    );

    assert!(
        !dismiss_merge_candidate(&db.pool, uuid::Uuid::now_v7(), None)
            .await
            .expect("dismiss unknown"),
        "an unknown id is a false, not an error"
    );
}

// ---------------------------------------------------------------------------
// merge_series — the irreversible one
// ---------------------------------------------------------------------------

/// A merge moves the whole footprint of one series onto another and then deletes it.
///
/// Eleven statements, each owning one table, and a dropped one loses whatever that table held —
/// silently, because the merge still reports success and the series row is still gone. Two of those
/// tables hold **user** data (`watchlist_entries`, `read_progress`), so this is the one operation in
/// the catalogue that can destroy something a user cannot re-create. There is no undo: the source
/// series is deleted in the same transaction.
///
/// Every claim below is a table: sources re-parent, the merged canonical title survives as an
/// alternative title (so the merged spelling still *matches* afterwards — otherwise the next scan
/// re-creates the series it just absorbed), tags and authors union, the watchlist entry moves, and
/// the external-sync mapping moves so the account does not silently stop syncing.
#[tokio::test]
async fn merging_moves_every_table_and_deletes_the_absorbed_series() {
    let db = TestDb::spawn().await;
    let alpha = seed::provider(&db, "alpha").create().await;
    let beta = seed::provider(&db, "beta").create().await;
    let keep = ingest(
        &db,
        alpha,
        &Seed {
            title: "Berserk",
            content_type: ContentType::Manga,
            release_year: Some(1989),
            tags: &["Action"],
            authors: &["Kentaro Miura"],
            alt_titles: &["Beruseruku"],
        },
        &[1.0, 2.0],
    )
    .await;
    let drop = ingest(
        &db,
        beta,
        &Seed {
            title: "Vinland Saga",
            content_type: ContentType::Manga,
            release_year: Some(2005),
            tags: &["Historical"],
            authors: &["Makoto Yukimura"],
            alt_titles: &["Vinrando Saga"],
        },
        &[1.0],
    )
    .await;
    assert_ne!(keep, drop);

    let user = seed::user(&db, "reader").create().await;
    watchlist_upsert(&db.pool, user, drop, WatchStatus::Reading, true)
        .await
        .expect("watchlist");
    sync::upsert_mapping(&db.pool, drop, "anilist", "12345")
        .await
        .expect("mapping");

    merge_series(&db.pool, keep, drop, Some(user))
        .await
        .expect("merge");

    // The absorbed series is gone and its source now hangs off the survivor.
    let series_ids: Vec<uuid::Uuid> = sqlx::query_scalar("SELECT id FROM series")
        .fetch_all(&db.pool)
        .await
        .expect("read series");
    assert_eq!(series_ids, vec![keep.as_uuid()]);
    let source_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM series_sources WHERE series_id = $1")
            .bind(keep.as_uuid())
            .fetch_one(&db.pool)
            .await
            .expect("count sources");
    assert_eq!(source_count, 2, "both providers' sources re-parented");

    // The absorbed canonical title and its alternatives survive as alternative titles, which is
    // what keeps the next scan of `beta` attaching here rather than re-creating the series.
    let mut titles = list_series_titles(&db.pool, keep).await.expect("titles");
    titles.sort();
    assert_eq!(
        titles,
        vec!["Beruseruku", "Vinland Saga", "Vinrando Saga"],
        "the merged work must remain findable by its own titles"
    );

    let mut tags: Vec<String> = sqlx::query_scalar(
        "SELECT t.name FROM series_tags st JOIN tags t ON t.id = st.tag_id WHERE st.series_id = $1",
    )
    .bind(keep.as_uuid())
    .fetch_all(&db.pool)
    .await
    .expect("read tags");
    tags.sort();
    assert_eq!(tags, vec!["Action", "Historical"]);

    let mut authors: Vec<String> = sqlx::query_scalar(
        "SELECT a.name FROM series_authors sa JOIN authors a ON a.id = sa.author_id \
         WHERE sa.series_id = $1",
    )
    .bind(keep.as_uuid())
    .fetch_all(&db.pool)
    .await
    .expect("read authors");
    authors.sort();
    assert_eq!(authors, vec!["Kentaro Miura", "Makoto Yukimura"]);

    let watched: Vec<SeriesId> = watchlist_list(&db.pool, user)
        .await
        .expect("watchlist")
        .iter()
        .map(|e| e.series_id)
        .collect();
    assert_eq!(
        watched,
        vec![keep],
        "a user must not lose a series from their watchlist to an operator merge"
    );

    assert_eq!(
        sync::mapping_series_for_external(&db.pool, "anilist", "12345")
            .await
            .expect("mapping"),
        Some(keep),
        "the external mapping must follow, or the account silently stops syncing this work"
    );
}

/// **MERGE-1.** A merge keeps the **furthest** read position when a user tracked both series, and
/// drops a part frontier the merged whole frontier has overtaken.
///
/// `ON CONFLICT DO UPDATE … GREATEST(...)` is the only thing standing between an operator merge and
/// a user losing reading progress, and the direction is not symmetric: taking the *absorbed* row's
/// value unconditionally, or letting the insert lose to the existing row, both compile and both
/// look right in a test where only one side has progress. Four users cover the four shapes — behind
/// on the survivor, ahead on the survivor, present on one side only, and a part frontier to
/// reconcile.
///
/// The last shape is the defect this found. The part-frontier `CASE` read
/// `whole >= floor(part) **AND** part = 0`, so it only cleared the frontier when there was no part
/// frontier at all and its staleness half could never fire: merging whole `6` with part `4.5`
/// produced `(6, 4.5)`, which §A.1 (`floor(part) >= whole`) forbids. Harmless to every current
/// reader — `covers` already calls `4.5` read from `floor(4.5) <= 6` — and self-healing on the next
/// `progress_set`, which is why nothing noticed; but the invariant is documented, so any future read
/// model that reports "you are ahead on 4.5" would have been reading a lie. All three write paths
/// now apply the same `floor(part) <= whole` rule.
#[tokio::test]
async fn merging_keeps_the_furthest_read_position() {
    let db = TestDb::spawn().await;
    let alpha = seed::provider(&db, "alpha").create().await;
    let beta = seed::provider(&db, "beta").create().await;
    let keep = ingest(
        &db,
        alpha,
        &Seed {
            title: "Berserk",
            content_type: ContentType::Manga,
            release_year: None,
            tags: &[],
            authors: &[],
            alt_titles: &[],
        },
        &[1.0, 2.0, 3.0, 4.5, 6.0],
    )
    .await;
    let drop = ingest(
        &db,
        beta,
        &Seed {
            title: "Vinland Saga",
            content_type: ContentType::Manga,
            release_year: None,
            tags: &[],
            authors: &[],
            alt_titles: &[],
        },
        &[1.0, 2.0, 3.0, 4.5, 6.0],
    )
    .await;

    // Behind on the survivor, ahead on the series being absorbed.
    let behind = a_reader(&db, "behind", (keep, 1.0), (drop, 3.0, None)).await;
    // Ahead on the survivor, behind on the absorbed one — the other direction.
    let ahead = a_reader(&db, "ahead", (keep, 3.0), (drop, 1.0, None)).await;
    // Progress only on the absorbed series: the plain insert path.
    let only_absorbed = seed::user(&db, "onlyabsorbed").create().await;
    progress_set(&db.pool, only_absorbed, drop, 2.0)
        .await
        .expect("progress");
    // A part frontier on the absorbed series that stays ahead of the merged whole frontier: it is
    // genuine reading and must survive.
    let part_survives = a_reader(&db, "partsurvives", (keep, 3.0), (drop, 1.0, Some(4.5))).await;
    // MERGE-1: the same shape, but the survivor's whole frontier *overtakes* the part. `(6, 4.5)`
    // is the §A.1 violation the old `AND` could not avoid.
    let part_is_stale = a_reader(&db, "partisstale", (keep, 6.0), (drop, 1.0, Some(4.5))).await;

    merge_series(&db.pool, keep, drop, None)
        .await
        .expect("merge");

    for (user, expected, why) in [
        (
            behind,
            (3.0, None),
            "the absorbed series' further position wins",
        ),
        (
            ahead,
            (3.0, None),
            "the survivor's further position is kept",
        ),
        (
            only_absorbed,
            (2.0, None),
            "progress with nothing to merge against",
        ),
        (
            part_survives,
            (3.0, Some(4.5)),
            "a part still ahead of the merged whole frontier is genuine reading",
        ),
        (
            part_is_stale,
            (6.0, None),
            "MERGE-1: floor(4.5) <= 6, so the part frontier is stale and §A.1 requires it cleared",
        ),
    ] {
        let progress: ReadProgress = progress_get_full(&db.pool, user, keep)
            .await
            .expect("read progress")
            .expect("a progress row");
        assert_eq!(
            (
                progress.last_read_whole_number,
                progress.last_read_part_number
            ),
            expected,
            "{why}"
        );
        // Whatever the frontiers are, they must satisfy §A.1 — that is the property a read model
        // is entitled to trust.
        if let Some(part) = progress.last_read_part_number {
            assert!(
                part.floor() > progress.last_read_whole_number,
                "§A.1: floor({part}) must be ahead of the whole frontier {} ({why})",
                progress.last_read_whole_number
            );
        }
        // And every chapter either side reads the same as `covers` says.
        assert!(
            progress.covers(1.0),
            "chapter 1 is read in every case ({why})"
        );
    }
}

/// A merge leaves no candidate naming the vanishing series, and refuses the two inputs that cannot
/// mean anything.
///
/// The property is "no orphan is left in the queue", and the mechanism is worth naming because it is
/// not the statement it looks like: both of `merge_candidates`' series columns are
/// `ON DELETE CASCADE`, so the rows are **deleted** by `DELETE FROM series`, and the
/// `UPDATE merge_candidates SET resolved = true` ahead of it is belt-and-braces for the same rows.
/// Either way `list_open_merge_candidates` inner-joins both sides, so a surviving row pointing at a
/// deleted series would simply *disappear from the operator's queue* while staying open in the table
/// — impossible to dismiss, and a queue that shrinks without anyone deciding. Asserting the property
/// rather than the statement is what keeps this test true if the FK is ever loosened.
///
/// A candidate naming only the *surviving* series is untouched: it is still a real question.
///
/// Merging a series into itself would run every union statement against one row and then delete it,
/// so the guard is what stops a mis-click from erasing a series outright; it is a `Conflict` rather
/// than a silent no-op so the console can say why.
#[tokio::test]
async fn merging_resolves_the_related_candidates_and_refuses_impossible_inputs() {
    let db = TestDb::spawn().await;
    let alpha = seed::provider(&db, "alpha").create().await;
    let beta = seed::provider(&db, "beta").create().await;
    let keep = ingest(
        &db,
        alpha,
        &Seed {
            title: "Berserk",
            content_type: ContentType::Manga,
            release_year: None,
            tags: &[],
            authors: &[],
            alt_titles: &[],
        },
        &[1.0],
    )
    .await;
    register_source_stub(&db.pool, beta, "/b/berserk", "Berserk", &always_ambiguous())
        .await
        .expect("register stub");
    let candidate = list_open_merge_candidates(&db.pool, 10)
        .await
        .expect("list")[0]
        .clone();
    let drop = candidate.series_id;

    // A third series, so one candidate names only the survivor and must be left alone.
    let unrelated = ingest(
        &db,
        alpha,
        &Seed {
            title: "Vinland Saga",
            content_type: ContentType::Manga,
            release_year: None,
            tags: &[],
            authors: &[],
            alt_titles: &[],
        },
        &[1.0],
    )
    .await;
    sqlx::query(
        "INSERT INTO merge_candidates (id, series_id, candidate_id, score, reason) \
         VALUES (gen_random_uuid(), $1, $2, 0.7, 'unrelated pair')",
    )
    .bind(keep.as_uuid())
    .bind(unrelated.as_uuid())
    .execute(&db.pool)
    .await
    .expect("seed an unrelated candidate");

    let actor = seed::user(&db, "operator").create().await;
    merge_series(&db.pool, keep, drop, Some(actor))
        .await
        .expect("merge");

    let open = list_open_merge_candidates(&db.pool, 10)
        .await
        .expect("list");
    assert_eq!(
        open.len(),
        1,
        "the pair naming the survivor only is still a real question"
    );
    assert_eq!(open[0].candidate_id, unrelated);
    let orphans: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM merge_candidates mc \
         WHERE NOT EXISTS (SELECT 1 FROM series s WHERE s.id = mc.series_id) \
            OR NOT EXISTS (SELECT 1 FROM series s WHERE s.id = mc.candidate_id)",
    )
    .fetch_one(&db.pool)
    .await
    .expect("count orphans");
    assert_eq!(
        orphans, 0,
        "no candidate may name a series that no longer exists"
    );
    assert!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM merge_candidates WHERE id = $1")
            .bind(candidate.id)
            .fetch_one(&db.pool)
            .await
            .expect("count the merged pair's candidate")
            == 0,
        "the cascade removes it; the resolve UPDATE ahead of the DELETE is belt-and-braces"
    );

    let into_itself = merge_series(&db.pool, keep, keep, None).await;
    assert!(
        matches!(into_itself, Err(DbError::Conflict(_))),
        "{into_itself:?}"
    );
    let unknown = merge_series(&db.pool, keep, SeriesId::new(), None).await;
    assert!(matches!(unknown, Err(DbError::NotFound)), "{unknown:?}");
    // The refusals must be refusals, not partial work.
    let mut series_ids: Vec<uuid::Uuid> = sqlx::query_scalar("SELECT id FROM series")
        .fetch_all(&db.pool)
        .await
        .expect("read series");
    series_ids.sort();
    let mut expected = vec![keep.as_uuid(), unrelated.as_uuid()];
    expected.sort();
    assert_eq!(series_ids, expected);
}
