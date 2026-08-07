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
    list_open_merge_candidates, merge_series, resolve_merged_series, resolve_merged_series_batch,
    revert_merge,
};
use tankovault_db::repo::sync;
use tankovault_db::repo::tracking::{
    ReadProgress, progress_get_full, progress_mark_read, progress_set, watchlist_list,
    watchlist_upsert,
};
use tankovault_domain::{
    ContentType, MetadataPriority, ProviderId, SeriesId, SeriesStatus, UserId, WatchStatus,
    normalize_title,
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
        &MetadataPriority::default(),
        &tankovault_domain::TagBlocklist::default(),
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

    let queued = list_open_merge_candidates(&db.pool, 10, 0.0)
        .await
        .expect("list merge candidates");
    assert_eq!(queued.len(), 1);
    let candidate = &queued[0];
    // The pair is stored in canonical id order, **not** creation order. `series_id` used to be
    // the newly-created series and `candidate_id` the one it might duplicate, which recorded
    // nothing but which side a scan happened to reach second — and made `(A,B)` and `(B,A)` two
    // different rows for one pair, so neither the unique index nor an operator's dismissal could
    // be relied on. Both ids are still here; which one survives a merge is decided separately,
    // from `suggested_keep`.
    assert_eq!(
        (
            candidate.series_id.min(candidate.candidate_id),
            candidate.series_id.max(candidate.candidate_id)
        ),
        (candidate.series_id, candidate.candidate_id),
        "the pair must be stored in canonical id order"
    );
    assert!(
        candidate.series_id == existing || candidate.candidate_id == existing,
        "the pre-existing series must be one side of the pair"
    );
    assert_eq!(candidate.series_title, "Berserk");
    assert_eq!(candidate.candidate_title, "Berserk");
    assert_eq!(candidate.reason.as_deref(), Some("ambiguous title match"));
    assert!(
        candidate.score > 0.0,
        "the operator judges by the score; {} is not a judgement",
        candidate.score
    );
    // The signals are what the console renders as badges and what the sweep re-judges by. A row
    // that records only a number is the state this queue was in for its whole life: 2 676 rows
    // all carrying the same "ambiguous title match" and nothing to distinguish them.
    assert!(
        !candidate.signals.is_empty(),
        "an ambiguous pair must record *why* it was ambiguous"
    );
    // Both counts come from `series_sources`, and `suggested_keep` has to be one of the two.
    assert!(
        candidate.suggested_keep == candidate.series_id
            || candidate.suggested_keep == candidate.candidate_id
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

/// The queue lists only unresolved candidates, **highest confidence first**, with both titles
/// resolved.
///
/// Three separate things, each of which fails quietly. `WHERE NOT mc.resolved` is the difference
/// between a work queue and a log — without it an operator re-decides everything they have already
/// decided. Both `JOIN series` legs supply the titles the console renders; an inner join is correct
/// here (a candidate referencing a deleted series is meaningless) but means a dropped join makes
/// rows *vanish* rather than error. And the ordering is what makes the queue drainable.
///
/// The ordering was `created_at DESC` and is now `score DESC, created_at DESC`. That is a
/// deliberate reversal of what this test used to assert, and the reason is scale: on a 26k-series
/// catalogue the queue reached 2 676 rows, so "newest first" put whatever the last scan happened
/// to observe at the top and buried every certain duplicate behind it. Confidence order is what
/// makes a queue that size drainable at all; `created_at DESC` remains the tie-break, so two
/// equally-confident pairs still arrive newest-first.
#[tokio::test]
async fn the_merge_queue_lists_only_unresolved_candidates_by_confidence() {
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

    let queued = list_open_merge_candidates(&db.pool, 10, 0.0)
        .await
        .expect("list");
    assert_eq!(queued.len(), 2);
    assert!(
        queued[0].score >= queued[1].score,
        "highest confidence first, or an operator drains the queue from the wrong end"
    );
    // Both pairs here score identically (the same title against the same series), so the
    // tie-break is what is actually being observed: newest first within a confidence level.
    assert!(queued[0].created_at >= queued[1].created_at);

    let limited = list_open_merge_candidates(&db.pool, 1, 0.0)
        .await
        .expect("list");
    assert_eq!(limited.len(), 1);
    assert_eq!(limited[0].id, queued[0].id);

    // The confidence filter narrows the queue rather than reordering it. An operator working a
    // band should not see a row below it at all.
    let unreachable = list_open_merge_candidates(&db.pool, 10, 1.01)
        .await
        .expect("list");
    assert!(
        unreachable.is_empty(),
        "no score can clear a threshold above the scorer's ceiling"
    );

    // Resolving one takes it out of the queue and leaves the other.
    let actor = seed::user(&db, "operator").create().await;
    assert!(
        dismiss_merge_candidate(&db.pool, queued[0].id, Some(actor))
            .await
            .expect("dismiss"),
        "dismissing an open candidate reports that it did something"
    );
    let remaining: Vec<_> = list_open_merge_candidates(&db.pool, 10, 0.0)
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
    let candidate = list_open_merge_candidates(&db.pool, 10, 0.0)
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

    merge_series(&db.pool, keep, drop, Some(user), "merged")
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

    merge_series(&db.pool, keep, drop, None, "merged")
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
#[expect(
    clippy::too_many_lines,
    reason = "one merge scenario asserted from several angles against a single seeded graph; \
              splitting it would re-seed a different graph per fragment and stop the \
              candidate-resolution and self-merge guards being observed on the same rows"
)]
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
    let candidate = list_open_merge_candidates(&db.pool, 10, 0.0)
        .await
        .expect("list")[0]
        .clone();
    // The pair is stored in canonical id order, so which column holds the newly-created series
    // is an accident of the uuids. Whichever one is not the survivor is the one being absorbed.
    let drop = if candidate.series_id == keep {
        candidate.candidate_id
    } else {
        candidate.series_id
    };

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
    merge_series(&db.pool, keep, drop, Some(actor), "merged")
        .await
        .expect("merge");

    let open = list_open_merge_candidates(&db.pool, 10, 0.0)
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

    let into_itself = merge_series(&db.pool, keep, keep, None, "merged").await;
    assert!(
        matches!(into_itself, Err(DbError::Conflict(_))),
        "{into_itself:?}"
    );
    let unknown = merge_series(&db.pool, keep, SeriesId::new(), None, "merged").await;
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

// ---------------------------------------------------------------------------
// The queue as a set of pairs, and the standing duplicate sweep
// ---------------------------------------------------------------------------

/// A `Seed` with nothing but a title, for the tests that only care about the title key.
const fn plain(title: &'static str) -> Seed {
    Seed {
        title,
        content_type: ContentType::Unknown,
        release_year: None,
        tags: &[],
        authors: &[],
        alt_titles: &[],
    }
}

/// Recording the same pair twice refreshes one row instead of adding a second, and does so in
/// either argument order.
///
/// `record_merge_candidate` was a bare `INSERT`, which failed in two opposite ways at once. The
/// same ambiguity observed twice inserted two rows, so a queue of N rows was not N pairs. And
/// `(A,B)` and `(B,A)` were two different rows describing one pair, so nothing that keyed on the
/// pair — a unique index, a dismissal, the sweep's "have I seen this?" check — could be relied on.
#[tokio::test]
async fn recording_a_pair_twice_refreshes_one_row_in_either_order() {
    use tankovault_db::repo::matching::{QueueOutcome, record_merge_candidate};

    let db = TestDb::spawn().await;
    let alpha = seed::provider(&db, "alpha").create().await;
    let a = ingest(&db, alpha, &plain("Berserk"), &[1.0]).await;
    let b = ingest(&db, alpha, &plain("Vinland Saga"), &[1.0]).await;

    assert_eq!(
        record_merge_candidate(&db.pool, a, b, 0.7, &["near_identical"], "first")
            .await
            .expect("record"),
        QueueOutcome::Added,
        "a new pair lengthens the queue"
    );
    assert_eq!(
        record_merge_candidate(&db.pool, b, a, 0.9, &["compact_identity"], "second")
            .await
            .expect("re-record reversed"),
        QueueOutcome::Refreshed,
        "the same pair the other way round is the same pair, re-scored rather than added"
    );
    assert_eq!(
        merge_candidate_count(&db).await,
        1,
        "one pair is one row, whichever order it arrives in"
    );

    let queued = list_open_merge_candidates(&db.pool, 10, 0.0)
        .await
        .expect("list");
    assert_eq!(queued.len(), 1);
    assert!((queued[0].score - 0.9).abs() < 1e-6, "{}", queued[0].score);
    assert_eq!(queued[0].signals, vec!["compact_identity".to_owned()]);
    assert_eq!(queued[0].reason.as_deref(), Some("second"));
}

/// **Regression: an operator's dismissal used to be undone by the next scan.**
///
/// Dismissing a candidate marks the row resolved, but nothing stopped a later
/// `record_merge_candidate` for the same pair inserting a *fresh open row* — so "these are two
/// different works" was a judgement the system silently discarded, and the same pair came back
/// every scan. The pair key plus the upsert's `NOT resolved` guard is what makes the judgement
/// durable; without either, this test fails by the queue containing an open row again.
#[tokio::test]
async fn a_dismissed_pair_is_not_resurrected_by_a_later_observation() {
    use tankovault_db::repo::matching::{QueueOutcome, record_merge_candidate};

    let db = TestDb::spawn().await;
    let alpha = seed::provider(&db, "alpha").create().await;
    let a = ingest(&db, alpha, &plain("Berserk"), &[1.0]).await;
    let b = ingest(&db, alpha, &plain("Vinland Saga"), &[1.0]).await;
    record_merge_candidate(&db.pool, a, b, 0.7, &[], "seen")
        .await
        .expect("record");

    let operator = seed::user(&db, "operator").create().await;
    let id = list_open_merge_candidates(&db.pool, 10, 0.0)
        .await
        .expect("list")[0]
        .id;
    assert!(
        dismiss_merge_candidate(&db.pool, id, Some(operator))
            .await
            .expect("dismiss")
    );

    assert_eq!(
        record_merge_candidate(&db.pool, a, b, 0.95, &["compact_identity"], "seen again")
            .await
            .expect("re-record after dismissal"),
        QueueOutcome::Unchanged,
        "re-observing a dismissed pair must report that it changed nothing"
    );
    assert!(
        list_open_merge_candidates(&db.pool, 10, 0.0)
            .await
            .expect("list")
            .is_empty(),
        "a dismissed pair must stay dismissed"
    );
    assert_eq!(merge_candidate_count(&db).await, 1);
}

/// A series row that already exists, with no canonicalisation involved.
///
/// The sweep's whole subject is rows the matcher *did not* connect, and the improved matcher now
/// connects the obvious cases at ingest — so going through `ingest_series` to build a duplicate
/// no longer produces one. This is the honest shape of what the sweep meets in production: rows
/// written before the normalization was corrected, and rows that only become recognisable once
/// enrichment has filled them in.
async fn insert_series_directly(db: &TestDb, title: &str) -> SeriesId {
    let id = SeriesId::new();
    sqlx::query("INSERT INTO series (id, canonical_title, normalized_title) VALUES ($1, $2, $3)")
        .bind(id.as_uuid())
        .bind(title)
        .bind(normalize_title(title))
        .execute(&db.pool)
        .await
        .expect("insert a series row directly");
    id
}

/// The standing sweep finds duplicates the create-time matcher never queued, and stops offering
/// pairs an operator has resolved.
///
/// `find_duplicate_pairs` blocks on the whitespace-insensitive title key. `Spy X Family` and
/// `Spyxfamily` have a trigram similarity of 0.5 and share no token at all, so *before* the
/// compact rule existed nothing at ingest could have connected them — this is the class of
/// duplicate that sat in the catalogue permanently, 59 pairs of it, without ever reaching the
/// review queue. The second row is inserted directly for exactly that reason: the matcher would
/// now attach it, and what the sweep has to handle is the rows written while it would not.
#[tokio::test]
async fn the_duplicate_sweep_blocks_on_the_compact_title_key() {
    use tankovault_db::repo::matching::{find_duplicate_pairs, suppress_pair};

    let db = TestDb::spawn().await;
    let alpha = seed::provider(&db, "alpha").create().await;
    let spaced = ingest(&db, alpha, &plain("Spy X Family"), &[1.0]).await;
    let squashed = insert_series_directly(&db, "Spyxfamily").await;
    // An unrelated series, to prove the blocking predicate is an equality and not "everything".
    ingest(&db, alpha, &plain("Vinland Saga"), &[1.0]).await;

    assert_ne!(spaced, squashed);
    assert_eq!(merge_candidate_count(&db).await, 0, "nothing queued them");

    let pairs = find_duplicate_pairs(&db.pool, 50).await.expect("pairs");
    let expected = (spaced.min(squashed), spaced.max(squashed));
    assert_eq!(pairs, vec![expected], "exactly the one colliding pair");

    // An operator's decision removes it from the shortlist permanently. Without this the sweep
    // re-proposes every pair a human has already judged, every hour, forever.
    let operator = seed::user(&db, "operator").create().await;
    suppress_pair(&db.pool, squashed, spaced, Some(operator))
        .await
        .expect("suppress");
    assert!(
        find_duplicate_pairs(&db.pool, 50)
            .await
            .expect("pairs")
            .is_empty(),
        "a resolved pair must not be re-proposed"
    );
}

/// An alternative title on either side is enough to block a pair together.
///
/// The same work listed under its romaji name on one provider and its english name on another is
/// the commonest cross-provider duplicate there is, and the canonical titles share nothing.
#[tokio::test]
async fn the_duplicate_sweep_blocks_on_alternative_titles_too() {
    use tankovault_db::repo::matching::find_duplicate_pairs;

    let db = TestDb::spawn().await;
    let alpha = seed::provider(&db, "alpha").create().await;
    let english = ingest(
        &db,
        alpha,
        &Seed {
            title: "Solo Leveling",
            content_type: ContentType::Unknown,
            release_year: None,
            tags: &[],
            authors: &[],
            alt_titles: &["Na Honjaman Level Up"],
        },
        &[1.0],
    )
    .await;
    // Inserted directly: the alias rule now attaches this at ingest, which is the point — the
    // sweep's subject is the rows that were written before it did.
    let romaji = insert_series_directly(&db, "Na Honjaman Level Up").await;

    let pairs = find_duplicate_pairs(&db.pool, 50).await.expect("pairs");
    assert!(
        pairs.contains(&(english.min(romaji), english.max(romaji))),
        "a canonical title matching another series' synonym must be shortlisted, got {pairs:?}"
    );
}

/// Attach an alternative title to a series that already exists.
async fn add_alt_title(db: &TestDb, series_id: SeriesId, title: &str) {
    sqlx::query("INSERT INTO series_titles (series_id, title, normalized) VALUES ($1, $2, $3)")
        .bind(series_id.as_uuid())
        .bind(title)
        .bind(normalize_title(title))
        .execute(&db.pool)
        .await
        .expect("insert an alternative title directly");
}

/// **Regression: one title key held by thousands of series turned the shortlist quadratic.**
///
/// An adapter briefly scraped a Madara summary block's *labels* into `series_titles`, so 5 509
/// series answered to the alternative title `Status`, 5 508 to `Alternative`, and so on. Blocking
/// is an equality and equality alone bounds nothing: six such keys are `n·(n-1)/2` pairs each,
/// which took the live shortlist from 4 352 pairs to 15 176 110 and pushed a byte-identical
/// duplicate to position 8.9 million of a list the sweep reads 500 of. It was therefore
/// unreachable, permanently, while looking for all the world like a scoring problem.
#[tokio::test]
async fn an_over_shared_title_key_is_not_a_blocking_key() {
    use tankovault_db::repo::matching::find_duplicate_pairs;

    let db = TestDb::spawn().await;
    // Above MAX_KEY_FANOUT (16). Every canonical title is unique, so the clique can only come
    // from the shared alias.
    let clique: Vec<SeriesId> = {
        let mut ids = Vec::new();
        for i in 0..20 {
            let id = insert_series_directly(&db, &format!("Unrelated Work {i}")).await;
            add_alt_title(&db, id, "Status").await;
            ids.push(id);
        }
        ids
    };

    // A real duplicate hiding inside the clique: two of its members also share a title that
    // nothing else answers to.
    add_alt_title(&db, clique[3], "Kimi No Na Wa").await;
    add_alt_title(&db, clique[17], "Kimi No Na Wa").await;
    let genuine = (clique[3].min(clique[17]), clique[3].max(clique[17]));

    let pairs = find_duplicate_pairs(&db.pool, 500).await.expect("pairs");
    assert_eq!(
        pairs,
        vec![genuine],
        "only the selective key may block; `Status` would contribute 190 pairs here"
    );
}

/// **Regression: the new-pair shortlist re-offered the same prefix on every run.**
///
/// It is ordered by `(lo, hi)` with a `LIMIT`, and it used to exclude only pairs an operator had
/// *resolved* — so a pair queued for review was still open, came back in the same prefix an hour
/// later, and displaced whatever sat behind it. The prefix never turned over; the only pairs that
/// ever left it were the handful auto-merged per run. Anything past the budget was unreachable
/// however obvious a duplicate it was.
#[tokio::test]
async fn the_shortlist_does_not_re_offer_a_pair_it_has_already_recorded() {
    use tankovault_db::repo::matching::{
        QueueOutcome, find_duplicate_pairs, open_merge_pairs, record_merge_candidate,
    };

    let db = TestDb::spawn().await;
    let spaced = insert_series_directly(&db, "Spy X Family").await;
    let squashed = insert_series_directly(&db, "Spyxfamily").await;
    let pair = (spaced.min(squashed), spaced.max(squashed));

    assert_eq!(
        find_duplicate_pairs(&db.pool, 50).await.expect("pairs"),
        vec![pair]
    );

    assert_eq!(
        record_merge_candidate(&db.pool, spaced, squashed, 0.8, &["near_identical"], "test")
            .await
            .expect("queue"),
        QueueOutcome::Added,
    );
    assert!(
        find_duplicate_pairs(&db.pool, 50)
            .await
            .expect("pairs")
            .is_empty(),
        "an open pair is the requeue path's business, not the new-pair shortlist's"
    );
    assert_eq!(
        open_merge_pairs(&db.pool, 50).await.expect("open"),
        vec![pair],
        "and the requeue path must be the one still holding it"
    );
}

/// A scorer's `distinct` verdict keeps the pair out of the new-pair shortlist, stays revisitable,
/// and never overwrites an operator's dismissal.
///
/// **Regression: the verdict used to delete the row**, so that a later sweep could reconsider the
/// pair — right in intent, wrong in mechanism. It also made the verdict invisible to
/// `find_duplicate_pairs`, which re-offered the pair immediately; combined with the `LIMIT` that
/// is how the shortlist stalled. The three properties asserted here are the ones that have to
/// hold simultaneously, and the last is the one that a naive "just record it" would break.
#[tokio::test]
async fn a_distinct_verdict_is_durable_revisitable_and_yields_to_an_operator() {
    use tankovault_db::repo::matching::{
        QueueOutcome, distinct_merge_pairs, find_duplicate_pairs, open_merge_pairs,
        record_distinct_pair, record_merge_candidate, suppress_pair,
    };

    let db = TestDb::spawn().await;
    let a = insert_series_directly(&db, "Kingdom of the Wind").await;
    let b = insert_series_directly(&db, "Kingdomofthewind").await;
    let pair = (a.min(b), a.max(b));

    // Nothing was open, so nothing was withdrawn.
    assert!(
        !record_distinct_pair(&db.pool, a, b, 0.4, &["near_identical"])
            .await
            .expect("record distinct"),
    );
    assert!(
        find_duplicate_pairs(&db.pool, 50)
            .await
            .expect("pairs")
            .is_empty(),
        "durable: the shortlist must not re-offer a pair it has already judged"
    );
    assert!(
        open_merge_pairs(&db.pool, 50)
            .await
            .expect("open")
            .is_empty(),
        "and it must not reach an operator, who has nothing to decide"
    );
    assert_eq!(
        distinct_merge_pairs(&db.pool, 50).await.expect("recheck"),
        vec![pair],
        "revisitable: the recheck rotation is where it lives now"
    );

    // Enrichment changes the evidence and the pair becomes reviewable again.
    assert_eq!(
        record_merge_candidate(&db.pool, a, b, 0.8, &["compact_identity"], "sweep")
            .await
            .expect("requeue"),
        QueueOutcome::Reopened,
        "a scorer-distinct row must be reopenable, and reported as lengthening the queue"
    );
    assert_eq!(
        open_merge_pairs(&db.pool, 50).await.expect("open"),
        vec![pair]
    );

    // An operator decides. That verdict outranks every later re-score.
    let operator = seed::user(&db, "operator").create().await;
    suppress_pair(&db.pool, a, b, Some(operator))
        .await
        .expect("dismiss");
    record_distinct_pair(&db.pool, a, b, 0.4, &["near_identical"])
        .await
        .expect("re-judge");
    assert!(
        record_merge_candidate(&db.pool, a, b, 0.9, &["compact_identity"], "sweep")
            .await
            .is_ok_and(|outcome| outcome == QueueOutcome::Unchanged),
        "a dismissal must survive both a re-judgement and a re-queue"
    );
    let outcome: String = sqlx::query_scalar("SELECT outcome FROM merge_candidates")
        .fetch_one(&db.pool)
        .await
        .expect("outcome");
    assert_eq!(outcome, "dismissed");
}

/// **Regression: a merge used to destroy four tables' worth of user data.**
///
/// `series_sync_overrides`, `sync_history`, `sync_remote_entries` and `notification_dedup` all
/// reference `series` and none of them was moved, so the `DELETE FROM series` at the end of the
/// merge cascaded them away: a user's per-series tracker exclusions and their whole visible sync
/// history vanished, every remote entry matched to the absorbed series came back *unmatched* (the
/// FK is `ON DELETE SET NULL`) and was re-resolved from the title on the next pull, and the
/// notification suppression went with it, so every watcher was re-notified for chapters that had
/// already been announced.
///
/// That was survivable while a merge was an operator pressing a button on a queue nobody worked.
/// It is not survivable now the sweep merges automatically.
#[tokio::test]
async fn a_merge_carries_the_sync_and_notification_state_across() {
    let db = TestDb::spawn().await;
    let alpha = seed::provider(&db, "alpha").create().await;
    let keep = ingest(&db, alpha, &plain("Berserk"), &[1.0]).await;
    let drop = ingest(&db, alpha, &plain("Vinland Saga"), &[1.0]).await;
    let user = seed::user(&db, "reader").create().await;

    sqlx::query(
        "INSERT INTO series_sync_overrides (user_id, series_id, provider, excluded) \
         VALUES ($1, $2, 'anilist', true)",
    )
    .bind(user.as_uuid())
    .bind(drop.as_uuid())
    .execute(&db.pool)
    .await
    .expect("seed an exclusion");
    sqlx::query(
        "INSERT INTO sync_history (user_id, series_id, provider, action, detail) \
         VALUES ($1, $2, 'anilist', 'push', '{}'::jsonb)",
    )
    .bind(user.as_uuid())
    .bind(drop.as_uuid())
    .execute(&db.pool)
    .await
    .expect("seed history");
    sqlx::query(
        "INSERT INTO sync_remote_entries \
            (user_id, provider, external_id, title, status, updated_at, series_id) \
         VALUES ($1, 'anilist', 'ext-1', 'Vinland Saga', 'current', now(), $2)",
    )
    .bind(user.as_uuid())
    .bind(drop.as_uuid())
    .execute(&db.pool)
    .await
    .expect("seed a remote entry");
    sqlx::query(
        "INSERT INTO notification_dedup (user_id, series_id, chapter_number) VALUES ($1, $2, 12)",
    )
    .bind(user.as_uuid())
    .bind(drop.as_uuid())
    .execute(&db.pool)
    .await
    .expect("seed dedup");

    merge_series(&db.pool, keep, drop, Some(user), "merged")
        .await
        .expect("merge");

    let excluded: Option<bool> = sqlx::query_scalar(
        "SELECT excluded FROM series_sync_overrides WHERE user_id = $1 AND series_id = $2",
    )
    .bind(user.as_uuid())
    .bind(keep.as_uuid())
    .fetch_optional(&db.pool)
    .await
    .expect("read the exclusion");
    assert_eq!(
        excluded,
        Some(true),
        "a user's decision not to sync a series must survive the catalogue merging it"
    );

    let history: i64 = sqlx::query_scalar("SELECT count(*) FROM sync_history WHERE series_id = $1")
        .bind(keep.as_uuid())
        .fetch_one(&db.pool)
        .await
        .expect("count history");
    assert_eq!(
        history, 1,
        "the user-visible sync log must move, not vanish"
    );

    let mapped: Option<uuid::Uuid> =
        sqlx::query_scalar("SELECT series_id FROM sync_remote_entries WHERE external_id = 'ext-1'")
            .fetch_one(&db.pool)
            .await
            .expect("read the remote entry");
    assert_eq!(
        mapped,
        Some(keep.as_uuid()),
        "an already-matched remote entry must be re-pointed, not orphaned back to unmatched"
    );

    let dedup: i64 =
        sqlx::query_scalar("SELECT count(*) FROM notification_dedup WHERE series_id = $1")
            .bind(keep.as_uuid())
            .fetch_one(&db.pool)
            .await
            .expect("count dedup");
    assert_eq!(
        dedup, 1,
        "notification suppression must move, or the merge re-announces old chapters"
    );
}

/// `tv_normalize_title` — the SQL twin the 0023 migration backfills with — must agree with
/// `tankovault_domain::normalize_title`, which is the authority.
///
/// The migration cannot call the Rust function and 26k stored keys cannot wait for a re-scan, so
/// the twin exists; this is the only thing that keeps it a twin rather than a second, diverging
/// implementation. Every case below is a rule the two have to implement identically, and the
/// first four are the ones that were actually wrong in production.
#[tokio::test]
async fn the_sql_normalizer_agrees_with_the_rust_one() {
    let db = TestDb::spawn().await;
    let corpus = [
        // The reported failure: an apostrophe joins a word, in every spelling.
        "Sorry but I\u{2019}m not Yuri",
        "Sorry But Im Not Yuri",
        "The Witch's Tears Become Poison",
        "God-Tier Extra\u{2019}s Ultimate Guide",
        // Combining marks and dotted capital I.
        "\u{0130}stanbul",
        "Be\u{0301}rserk",
        "B\u{e9}rserk",
        // Full-width forms, as a Japanese input method emits them.
        "\u{ff33}\u{ff30}\u{ff39}\u{d7}\u{ff26}\u{ff21}\u{ff2d}\u{ff29}\u{ff2c}\u{ff39}",
        "\u{ff36}\u{ff2f}\u{ff2c}\u{ff0e}\u{ff11}\u{ff12}",
        // Multi-letter folds, an ampersand, and the noise-word fallback.
        "Stra\u{df}e",
        "Strasse",
        "\u{c6}on",
        "Ao & Haru",
        "Tom&Jerry",
        "Solo Leveling Manhwa",
        "Berserk (Official Scan)",
        "Manga",
        "   ",
        "Re:Zero  -  Starting Life!",
        // Scripts the catch-all arm must keep verbatim.
        "\u{30ef}\u{30f3}\u{30d4}\u{30fc}\u{30b9}",
        "\u{b098} \u{d63c}\u{c790}\u{b9cc} \u{b808}\u{bca8}\u{c5c5}",
        "\u{14c}oku",
    ];
    for title in corpus {
        let sql: String = sqlx::query_scalar("SELECT tv_normalize_title($1)")
            .bind(title)
            .fetch_one(&db.pool)
            .await
            .expect("call tv_normalize_title");
        assert_eq!(
            sql,
            normalize_title(title),
            "SQL and Rust normalizers disagree on {title:?}"
        );
    }
}

/// Rebuilding the keys is the authoritative repair, and it is safe to run twice.
///
/// The normalized title is written once, at creation, so a change to the normalization rules
/// leaves the whole catalogue on the old keys until this runs. The second run asserting zero
/// updates is what makes it an operator-safe button rather than a one-shot migration.
#[tokio::test]
async fn rebuilding_the_keys_is_idempotent_and_repairs_a_stale_key() {
    use tankovault_db::repo::matching::rebuild_normalized_keys;

    let db = TestDb::spawn().await;
    let alpha = seed::provider(&db, "alpha").create().await;
    let series = ingest(&db, alpha, &plain("Sorry but I\u{2019}m not Yuri"), &[1.0]).await;

    // Simulate a row written under the previous rules, where an apostrophe was a separator.
    sqlx::query("UPDATE series SET normalized_title = 'sorry but i m not yuri' WHERE id = $1")
        .bind(series.as_uuid())
        .execute(&db.pool)
        .await
        .expect("stale the key");

    let first = rebuild_normalized_keys(&db.pool, normalize_title)
        .await
        .expect("rebuild");
    assert_eq!(first.series_updated, 1, "{first:?}");

    let key: String = sqlx::query_scalar("SELECT normalized_title FROM series WHERE id = $1")
        .bind(series.as_uuid())
        .fetch_one(&db.pool)
        .await
        .expect("read the key");
    assert_eq!(key, "sorry but im not yuri");

    let second = rebuild_normalized_keys(&db.pool, normalize_title)
        .await
        .expect("rebuild again");
    assert_eq!(second.series_updated, 0, "a second pass must write nothing");
    assert_eq!(second.titles_updated, 0);
    assert_eq!(second.series_scanned, first.series_scanned);
}

// ---------------------------------------------------------------------------
// Merges: the alias map, and the guard on `merge_series` itself
// ---------------------------------------------------------------------------

/// Three distinct works, so a merge in these tests is never also a *match* — the point under
/// test is what the merge moves, not what the canonicaliser would have decided.
const BERSERK: Seed = Seed {
    title: "Berserk",
    content_type: ContentType::Manga,
    release_year: Some(1989),
    tags: &["Action"],
    authors: &["Kentaro Miura"],
    alt_titles: &[],
};
const VINLAND: Seed = Seed {
    title: "Vinland Saga",
    content_type: ContentType::Manga,
    release_year: Some(2005),
    tags: &["Historical"],
    authors: &["Makoto Yukimura"],
    alt_titles: &[],
};
const MONSTER: Seed = Seed {
    title: "Monster",
    content_type: ContentType::Manga,
    release_year: Some(1994),
    tags: &["Psychological"],
    authors: &["Naoki Urasawa"],
    alt_titles: &[],
};

/// What [`merge_series`] does with one column that points at a series.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Handling {
    /// Re-pointed, unioned or resolved onto the survivor inside the merge transaction.
    Folded,
    /// Deliberately allowed to vanish with the absorbed row. The string is *why* — a reason
    /// somebody has to write down, which is the whole point of the distinction.
    Cascades(&'static str),
}

/// Every column that points at `series(id)`, and what the merge does with it.
///
/// This is the hand-maintained half of
/// [`every_column_pointing_at_a_series_is_folded_or_deliberately_cascaded`]; the schema is the
/// other half. Adding a table with a `series_id` and nothing else makes that test fail until
/// somebody decides which of these two this row is.
const SERIES_REFERENCES: &[(&str, &str, Handling)] = &[
    ("series_titles", "series_id", Handling::Folded),
    ("series_tags", "series_id", Handling::Folded),
    ("series_authors", "series_id", Handling::Folded),
    ("series_sources", "series_id", Handling::Folded),
    ("watchlist_entries", "series_id", Handling::Folded),
    ("read_progress", "series_id", Handling::Folded),
    ("notification_dedup", "series_id", Handling::Folded),
    ("sync_mappings", "series_id", Handling::Folded),
    ("sync_remote_entries", "series_id", Handling::Folded),
    ("series_sync_overrides", "series_id", Handling::Folded),
    ("sync_conflicts", "series_id", Handling::Folded),
    ("sync_history", "series_id", Handling::Folded),
    ("merge_candidates", "series_id", Handling::Folded),
    ("merge_candidates", "candidate_id", Handling::Folded),
    ("series_merges", "survivor_id", Handling::Folded),
    // The recommendation model. Every one of these is *derived* — a rebuild reproduces it from
    // `series` and its link tables — so losing the absorbed series' rows is not only acceptable,
    // it is the mechanism: the cascade is what makes a merged series unreachable from the index
    // in the same transaction that deletes it, rather than at the next build.
    //
    // What is *not* automatic is the survivor, which absorbed the loser's tags and authors and
    // therefore needs re-embedding. `merge_series` queues it; see `rec_repair_queue` below.
    (
        "series_features",
        "series_id",
        Handling::Cascades(
            "derived from the series' tags, authors and scalars; the survivor is queued for re-extraction",
        ),
    ),
    (
        "series_embedding",
        "series_id",
        Handling::Cascades(
            "derived from series_features; the cascade is what makes a merged series unreachable from the ANN index immediately",
        ),
    ),
    (
        "series_prior",
        "series_id",
        Handling::Cascades(
            "derived appeal signals, recomputed for the whole catalogue by every build",
        ),
    ),
    (
        "series_cooccurrence",
        "series_id",
        Handling::Cascades(
            "derived from reader lists; re-aggregated wholesale. Re-pointing would violate the series_id <> other_id CHECK for a (loser, survivor) pair and double-count support for every pair both shared",
        ),
    ),
    (
        "series_cooccurrence",
        "other_id",
        Handling::Cascades("the other half of the same pair; see series_cooccurrence.series_id"),
    ),
    ("rec_repair_queue", "series_id", Handling::Folded),
    // The reader model. Affinity is derived from the watchlist and read progress, both of which
    // this transaction folds correctly a few statements earlier — so re-pointing it by hand is
    // how the derived rows and their source diverge. `merge_series` marks the affected profiles
    // stale instead, and they are recomputed from the folded truth.
    (
        "user_series_affinity",
        "series_id",
        Handling::Cascades(
            "derived from watchlist_entries and read_progress, which are folded; affected taste profiles are marked stale and recomputed",
        ),
    ),
    // The one table here that holds a *decision* rather than a derivation.
    ("recommendation_feedback", "series_id", Handling::Folded),
    // The decision journals. `sync_decisions.series_id` is `ON DELETE SET NULL` rather than a
    // cascade — a journal a deletion can erase is not one — but the merge re-points it anyway,
    // because a record detached from the series it describes survives and stops being findable.
    ("sync_decisions", "series_id", Handling::Folded),
    ("sync_match_blocks", "series_id", Handling::Folded),
];

/// `user_taste_profile.seeds` is a `uuid[]` and therefore invisible to the enumeration above —
/// no foreign key, and not named like a series id.
///
/// That is a deliberate blind spot with a deliberate answer: the array is a *cache* of the top of
/// the affinity ordering, `merge_series` marks every affected profile stale, and a stale profile
/// is rebuilt from scratch before it is next read. A dangling id in it therefore survives exactly
/// until the next shelf request, and never reaches a reader.
///
/// Recorded here so the next person to add an array of series ids has to decide the same
/// question rather than discover it. An array that is *not* rebuilt on staleness would need a
/// real answer — which is the argument that kept the item model's neighbours out of arrays
/// entirely (docs/RECOMMENDATIONS.md §5.2).
const _ARRAY_COLUMNS_ARE_NOT_ENUMERATED: () = ();

/// **The guard on `merge_series`.**
///
/// `merge_series` is one long transaction that hand-folds every table holding a `series_id` and
/// then deletes the absorbed row. Nothing in the type system relates that list to the schema, so
/// a new table simply loses its rows on the next merge — silently, because a cascade and a
/// deliberate decision produce identical output.
///
/// Merges are frequent here (`merge_candidates.outcome` admits `auto_merged`, and the duplicate
/// sweep merges without an operator), so "silently, on some rows, eventually" means "constantly".
///
/// The test enumerates the schema two ways — every foreign key onto `series(id)`, and every
/// column *named* like a series id, which catches a new table that forgot the key as well — and
/// requires each to be classified. It asserts nothing about behaviour; it asserts that somebody
/// thought about it.
#[tokio::test]
async fn every_column_pointing_at_a_series_is_folded_or_deliberately_cascaded() {
    let db = TestDb::spawn().await;

    let rows = sqlx::query_as::<_, (String, String)>(
        "SELECT c.conrelid::regclass::text, a.attname \
           FROM pg_constraint c \
           JOIN unnest(c.conkey) AS k(attnum) ON true \
           JOIN pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = k.attnum \
          WHERE c.contype = 'f' AND c.confrelid = 'series'::regclass \
         UNION \
         SELECT table_name, column_name \
           FROM information_schema.columns \
          WHERE table_schema = 'public' AND column_name LIKE '%series_id%'",
    )
    .fetch_all(&db.pool)
    .await
    .expect("enumerate the columns that point at a series");

    let classified: std::collections::BTreeSet<(&str, &str)> = SERIES_REFERENCES
        .iter()
        .map(|(table, column, _)| (*table, *column))
        .collect();
    let live: std::collections::BTreeSet<(String, String)> = rows.into_iter().collect();

    let unclassified: Vec<&(String, String)> = live
        .iter()
        .filter(|(table, column)| !classified.contains(&(table.as_str(), column.as_str())))
        .collect();
    assert!(
        unclassified.is_empty(),
        "these columns point at a series and nothing says what a merge does with them: \
         {unclassified:?}\n\
         Add each to `SERIES_REFERENCES` as `Handling::Folded` — and fold it in `merge_series` \
         — or as `Handling::Cascades(reason)` if losing the rows with the absorbed series is \
         genuinely correct. See docs/RECOMMENDATIONS.md §9.4."
    );

    let stale: Vec<(&str, &str)> = SERIES_REFERENCES
        .iter()
        .map(|(table, column, _)| (*table, *column))
        .filter(|(table, column)| !live.contains(&((*table).to_owned(), (*column).to_owned())))
        .collect();
    assert!(
        stale.is_empty(),
        "`SERIES_REFERENCES` classifies columns the schema no longer has: {stale:?}. \
         Delete the rows; a classification that matches nothing hides the next real one."
    );

    // `series_merges.merged_id` is deliberately in neither list: it names a row that has already
    // been deleted, which is the entire purpose of the table, so it can carry no foreign key.
    // Asserted rather than assumed — a well-meaning future migration "fixing" the missing key
    // would make every merge fail on its own forwarding record.
    let merged_id_has_fk: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_constraint c \
                          JOIN unnest(c.conkey) AS k(attnum) ON true \
                          JOIN pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = k.attnum \
                         WHERE c.contype = 'f' AND c.conrelid = 'series_merges'::regclass \
                           AND a.attname = 'merged_id')",
    )
    .fetch_one(&db.pool)
    .await
    .expect("inspect series_merges");
    assert!(
        !merged_id_has_fk,
        "`series_merges.merged_id` must NOT reference `series(id)`: it names the row the merge \
         just deleted, so a foreign key there makes every merge fail."
    );
}

/// A merge records where the series went.
///
/// Before `series_merges` existed the absorbed row was deleted with no forwarding record, so
/// every merged id became a hard 404 — for bookmarks, shared links, external references and any
/// client holding a stale id. With the duplicate sweep merging automatically and continuously,
/// that was not a rare case.
#[tokio::test]
async fn a_merge_records_where_the_series_went() {
    let db = TestDb::spawn().await;
    let alpha = seed::provider(&db, "alpha").create().await;
    let beta = seed::provider(&db, "beta").create().await;
    let keep = ingest(&db, alpha, &BERSERK, &[1.0]).await;
    let drop = ingest(&db, beta, &VINLAND, &[1.0]).await;

    merge_series(&db.pool, keep, drop, None, "merged")
        .await
        .expect("merge");

    assert_eq!(
        resolve_merged_series(&db.pool, drop)
            .await
            .expect("resolve"),
        Some(keep),
        "the absorbed id must forward to the survivor"
    );
    assert_eq!(
        resolve_merged_series(&db.pool, keep)
            .await
            .expect("resolve"),
        None,
        "a live series has no forwarding address"
    );
}

/// **Path compression: the alias map stays one hop deep.**
///
/// Merge B into C after A was already merged into B, and A must resolve straight to C. If
/// `merge_series` only inserted its own row, A would still point at B — an id that no longer
/// exists — and resolution would need a recursive walk that is both slower and able to spin on a
/// cycle. The compression is one `UPDATE`; this test is what says it happened.
#[tokio::test]
async fn merging_a_survivor_repoints_the_aliases_that_pointed_at_it() {
    let db = TestDb::spawn().await;
    let alpha = seed::provider(&db, "alpha").create().await;
    let beta = seed::provider(&db, "beta").create().await;
    let gamma = seed::provider(&db, "gamma").create().await;

    let a = ingest(&db, alpha, &VINLAND, &[1.0]).await;
    let b = ingest(&db, beta, &BERSERK, &[1.0]).await;
    let c = ingest(&db, gamma, &MONSTER, &[1.0]).await;

    merge_series(&db.pool, b, a, None, "merged")
        .await
        .expect("merge a into b");
    merge_series(&db.pool, c, b, None, "merged")
        .await
        .expect("merge b into c");

    assert_eq!(
        resolve_merged_series(&db.pool, a).await.expect("resolve a"),
        Some(c),
        "A was merged into B and B into C, so A must resolve directly to C in one hop"
    );
    assert_eq!(
        resolve_merged_series(&db.pool, b).await.expect("resolve b"),
        Some(c)
    );

    // The batch form is what the request path uses, and it must agree with the single lookup.
    let mut batch = resolve_merged_series_batch(&db.pool, &[a, b, c])
        .await
        .expect("batch resolve");
    batch.sort_by_key(|(from, _)| from.as_uuid());
    let mut expected = vec![(a, c), (b, c)];
    expected.sort_by_key(|(from, _)| from.as_uuid());
    assert_eq!(
        batch, expected,
        "the batch form must resolve exactly the ids that moved, and no others"
    );
}

// ---------------------------------------------------------------------------
// The undo journal: a merge that can be taken back
// ---------------------------------------------------------------------------

/// A merge and its revert leave the database byte-identical to how it started.
///
/// This is the assertion the whole undo journal exists for, and it is deliberately made by
/// *comparing the database to itself* rather than by checking a list of tables: a per-table
/// assertion is exactly the thing that goes stale when someone adds a table, which is the failure
/// mode `every_column_pointing_at_a_series_is_folded_or_deliberately_cascaded` already exists to
/// catch on the forward path. Here the same risk runs backwards — a table the merge folds and the
/// revert does not restore — and the only check that cannot rot is the whole-content one.
///
/// The fixture is chosen so every shape of change in the journal is exercised at once:
///
/// - a row only the **absorbed** series had (a second reader's watchlist entry, its own source),
/// - a row only the **survivor** had (the first reader's watchlist entry),
/// - a row **both** had at the same key, where the merge overwrote the survivor's value with
///   `GREATEST` (the first reader's progress, 5.0 against 9.0) — the one case a `RETURNING`-based
///   journal cannot reconstruct,
/// - and a title present on both sides, so the union insert has something to skip and the revert
///   has something it must *not* delete.
#[tokio::test]
async fn a_merge_and_its_revert_leave_the_database_exactly_as_it_was() {
    let db = TestDb::spawn().await;
    let alpha = seed::provider(&db, "alpha").create().await;
    let beta = seed::provider(&db, "beta").create().await;
    let keep = ingest(&db, alpha, &BERSERK, &[1.0, 2.0]).await;
    let drop = ingest(&db, beta, &VINLAND, &[1.0]).await;

    let reader = seed::user(&db, "reader").create().await;
    let other = seed::user(&db, "other").create().await;

    // Both sides, same reader, different frontiers: the merge keeps the further one, so the
    // survivor's row is *overwritten* rather than created.
    watchlist_upsert(&db.pool, reader, keep, WatchStatus::Reading, true)
        .await
        .expect("survivor watchlist");
    progress_set(&db.pool, reader, keep, 5.0)
        .await
        .expect("survivor progress");
    progress_set(&db.pool, reader, drop, 9.0)
        .await
        .expect("absorbed progress");
    // A row only the absorbed series has, so the revert has something to move back off the
    // survivor entirely rather than merely restore.
    watchlist_upsert(&db.pool, other, drop, WatchStatus::Completed, false)
        .await
        .expect("absorbed watchlist");
    sync::upsert_mapping(&db.pool, drop, "anilist", "12345")
        .await
        .expect("mapping");
    // A title the survivor already answers to, so the union insert skips it and the revert must
    // not take it away.
    sqlx::query(
        "INSERT INTO series_titles (series_id, title, normalized) \
         VALUES ($1, $2, $3), ($4, $2, $3) ON CONFLICT DO NOTHING",
    )
    .bind(keep.as_uuid())
    .bind("Shared Alias")
    .bind(normalize_title("Shared Alias"))
    .bind(drop.as_uuid())
    .execute(&db.pool)
    .await
    .expect("shared alias");

    let before = snapshot(&db).await;

    let undo = merge_series(&db.pool, keep, drop, Some(reader), "auto_merged")
        .await
        .expect("merge");

    // Sanity: the merge really did something, or the comparison below proves nothing.
    let after_merge = snapshot(&db).await;
    assert_ne!(before, after_merge, "the merge must have changed something");
    assert!(
        undo.row_count() > 0,
        "the journal must carry the rows it will put back"
    );

    revert_merge(&db.pool, &undo).await.expect("revert");

    let after_revert = snapshot(&db).await;
    for (table, rows) in &before {
        assert_eq!(
            after_revert.get(table),
            Some(rows),
            "`{table}` differs after a merge and its revert"
        );
    }
    assert_eq!(before, after_revert, "the revert must restore everything");
}

/// The absorbed series comes back under its **original id**, not a new one.
///
/// Every bookmark, external mapping and shared link names an id. A revert that re-created the
/// series under a fresh id would restore the data and break exactly the references the merge
/// broke — which is the harm the whole feature exists to undo.
#[tokio::test]
async fn a_revert_restores_the_original_id_and_removes_the_forwarding_address() {
    let db = TestDb::spawn().await;
    let alpha = seed::provider(&db, "alpha").create().await;
    let beta = seed::provider(&db, "beta").create().await;
    let keep = ingest(&db, alpha, &BERSERK, &[1.0]).await;
    let drop = ingest(&db, beta, &VINLAND, &[1.0]).await;

    let undo = merge_series(&db.pool, keep, drop, None, "auto_merged")
        .await
        .expect("merge");
    assert_eq!(
        resolve_merged_series(&db.pool, drop)
            .await
            .expect("resolve"),
        Some(keep),
        "sanity: the merge left a forwarding address"
    );

    revert_merge(&db.pool, &undo).await.expect("revert");

    let live: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM series WHERE id = $1)")
        .bind(drop.as_uuid())
        .fetch_one(&db.pool)
        .await
        .expect("check the restored series");
    assert!(
        live,
        "the absorbed series must exist again under its own id"
    );
    assert_eq!(
        resolve_merged_series(&db.pool, drop)
            .await
            .expect("resolve"),
        None,
        "a restored series must not still forward to the survivor"
    );
}

/// Reverting twice is refused rather than half-applied.
///
/// The journal is not idempotent — it re-inserts rows and re-points ids — so applying it to a
/// database that has already been restored would fail partway through on a primary key. Refusing
/// on the live id is what turns that into a clean 409.
#[tokio::test]
async fn a_second_revert_is_refused() {
    let db = TestDb::spawn().await;
    let alpha = seed::provider(&db, "alpha").create().await;
    let beta = seed::provider(&db, "beta").create().await;
    let keep = ingest(&db, alpha, &BERSERK, &[1.0]).await;
    let drop = ingest(&db, beta, &VINLAND, &[1.0]).await;

    let undo = merge_series(&db.pool, keep, drop, None, "auto_merged")
        .await
        .expect("merge");
    revert_merge(&db.pool, &undo).await.expect("first revert");

    let again = revert_merge(&db.pool, &undo).await;
    assert!(
        matches!(again, Err(DbError::Conflict(_))),
        "a second revert must be a conflict, not a partial re-application: {again:?}"
    );
}

/// Every row of every table a revert is expected to restore, as text, keyed by table.
///
/// Read through `to_jsonb` so the comparison is over *values* rather than over a hand-written
/// column list — the same reason the journal itself round-trips through the composite type.
/// Sorted inside each table so two equal sets compare equal regardless of physical row order,
/// which changes freely when a row is deleted and re-inserted.
///
/// # What this deliberately does not cover
///
/// The `Handling::Cascades` tables are the recommender's *derived* rows: the merge lets them go
/// with the absorbed series and the revert re-queues both series for a rebuild rather than
/// reconstructing them, because re-deriving is the only way to be sure they agree with the
/// restored truth. Asserting they came back would be asserting the wrong thing.
///
/// `rec_repair_queue` is excluded for the opposite reason — it is a work queue, not state. The
/// merge enqueues the survivor and the revert enqueues both, and neither can un-request a rebuild
/// that has already been asked for.
async fn snapshot(db: &TestDb) -> std::collections::BTreeMap<String, Vec<String>> {
    // Derived from `SERIES_REFERENCES` rather than listed again, so a table classified as folded
    // there is compared here without this function changing. Plus the series row itself and the
    // forwarding map, which that list does not name.
    let mut tables: std::collections::BTreeSet<&str> = SERIES_REFERENCES
        .iter()
        .filter(|(table, _, handling)| {
            matches!(handling, Handling::Folded) && *table != "rec_repair_queue"
        })
        .map(|(table, _, _)| *table)
        .collect();
    tables.insert("series");
    tables.insert("series_merges");

    let mut out = std::collections::BTreeMap::new();
    for table in tables {
        // `AssertSqlSafe` because the interpolated name is a `&'static str` from
        // `SERIES_REFERENCES`, not anything a caller supplies.
        let mut rows: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT to_jsonb(t)::text FROM {table} t"
        )))
        .fetch_all(&db.pool)
        .await
        .unwrap_or_else(|e| panic!("snapshot {table}: {e}"));
        rows.sort_unstable();
        out.insert(table.to_owned(), rows);
    }
    out
}
