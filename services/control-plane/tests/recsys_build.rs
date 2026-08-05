//! The recommendation model, end to end: a real catalogue in, an ANN index out.
//!
//! The pure halves are unit-tested where they live — extraction and the digest in
//! `crates/recsys/src/features.rs`, the projection in `embedding.rs`, scoring in
//! `similarity.rs`. What none of those can show is whether the *pipeline* agrees with itself:
//! that the ids extraction interned are the ones the vocabulary pass counted, that the
//! `dense_index` the basis was solved against is the one the projection reads, and that a vector
//! written as `halfvec` comes back ranking the way the cosine said it would. Every one of those
//! seams is between two components that each pass their own tests.
//!
//! Opt-in: gated behind the `integration` feature because it requires Docker.
#![cfg(feature = "integration")]

use tankovault_config::MatchingConfig;
use tankovault_control_plane::recsys::{BuildTuning, build};
use tankovault_db::repo::catalog::{ChapterUpsert, ScannedSeries, SeriesUpsert, ingest_series};
use tankovault_db::repo::matching::merge_series;
use tankovault_db::repo::recsys;
use tankovault_domain::{
    ContentType, MetadataPriority, ProviderId, SeriesId, SeriesStatus, normalize_title,
};
use tankovault_test_support::{TestDb, seed};

/// Small batches on purpose: the paging is part of what this exercises, and a batch larger than
/// the fixture would walk the whole catalogue in one page and prove nothing about the cursor.
fn budget() -> BuildTuning {
    let mut tuning = BuildTuning::defaults(3, 100);
    tuning.budget.dense_input_cap = 64;
    tuning
}

/// A series with the tags and authors that decide where it lands.
struct Fixture {
    title: &'static str,
    tags: &'static [&'static str],
    authors: &'static [&'static str],
    content_type: ContentType,
}

/// Two clusters that share no tag and no author, plus enough chapters to clear the
/// recommendable gate. The separation is the assertion: a model that cannot tell these apart
/// cannot tell anything apart.
const DUNGEON: &[Fixture] = &[
    Fixture {
        title: "Solo Leveling",
        tags: &["action", "dungeon", "regression", "leveling"],
        authors: &["chugong"],
        content_type: ContentType::Manhwa,
    },
    Fixture {
        title: "The Gamer",
        tags: &["action", "dungeon", "leveling", "system"],
        authors: &["sung-sang-young"],
        content_type: ContentType::Manhwa,
    },
    Fixture {
        title: "Tower of God",
        tags: &["action", "dungeon", "system", "tower"],
        authors: &["slime"],
        content_type: ContentType::Manhwa,
    },
];

const ROMANCE: &[Fixture] = &[
    Fixture {
        title: "Fruits Basket",
        tags: &["romance", "slice-of-life", "supernatural", "drama"],
        authors: &["natsuki-takaya"],
        content_type: ContentType::Manga,
    },
    Fixture {
        title: "Kimi ni Todoke",
        tags: &["romance", "slice-of-life", "school", "drama"],
        authors: &["karuho-shiina"],
        content_type: ContentType::Manga,
    },
    Fixture {
        title: "Ao Haru Ride",
        tags: &["romance", "school", "drama", "slice-of-life"],
        authors: &["io-sakisaka"],
        content_type: ContentType::Manga,
    },
];

async fn ingest(db: &TestDb, provider: ProviderId, fixture: &Fixture) -> SeriesId {
    ingest_series(
        &db.pool,
        &ScannedSeries {
            provider_id: provider,
            source_path: format!("/s/{}", normalize_title(fixture.title).replace(' ', "-")),
            provider_title: Some(fixture.title.to_owned()),
            meta: SeriesUpsert {
                canonical_title: fixture.title.to_owned(),
                normalized_title: normalize_title(fixture.title),
                description: None,
                cover_url: None,
                content_type: fixture.content_type,
                status: SeriesStatus::Ongoing,
                release_year: Some(2015),
            },
            alt_titles: Vec::new(),
            tags: fixture.tags.iter().map(|t| (*t).to_owned()).collect(),
            authors: fixture.authors.iter().map(|a| (*a).to_owned()).collect(),
            // Enough to clear the length gate in `is_recommendable`.
            chapters: (1..=12)
                .map(|n| ChapterUpsert {
                    number: f64::from(n),
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
    )
    .await
    .expect("ingest series")
    .series_id
}

async fn catalogue(db: &TestDb) -> (Vec<SeriesId>, Vec<SeriesId>) {
    let provider = seed::provider(db, "alpha").create().await;
    let mut dungeon = Vec::new();
    for fixture in DUNGEON {
        dungeon.push(ingest(db, provider, fixture).await);
    }
    let mut romance = Vec::new();
    for fixture in ROMANCE {
        romance.push(ingest(db, provider, fixture).await);
    }
    (dungeon, romance)
}

/// **The pipeline agrees with itself.**
///
/// One full build over a six-series catalogue, then the assertion that matters: an ANN search
/// seeded with a dungeon series ranks the other dungeon series above every romance one. That is
/// only true if extraction, interning, the idf pass, the `dense_index` assignment, the basis and
/// the `halfvec` round trip all line up — each of which passes its own tests in isolation.
#[tokio::test]
async fn a_full_build_produces_an_index_that_separates_the_clusters() {
    let db = TestDb::spawn().await;
    let (dungeon, romance) = catalogue(&db).await;

    let report = build(&db.pool, budget(), true)
        .await
        .expect("build")
        .expect("the build was not claimed by anyone else");
    assert_eq!(
        report.generation, 1,
        "the first full build takes generation 1"
    );
    assert_eq!(report.series_built, 6);
    assert!(
        report.vocabulary > 0,
        "the vocabulary must have been counted"
    );
    assert!(report.dense_dims > 0, "a basis must have been solved");

    let state = recsys::read_build_state(&db.pool).await.expect("state");
    assert_eq!(state.stage, "idle", "the claim must be released");
    assert_eq!(state.error, None);

    let seed_id = dungeon[0];
    let embedding = recsys::embedding_of(&db.pool, seed_id)
        .await
        .expect("embedding")
        .expect("the seed must have been embedded");

    let neighbours = recsys::nearest_neighbours(&db.pool, &embedding, seed_id, 5, 20)
        .await
        .expect("ann search");
    assert!(!neighbours.is_empty(), "the index must return something");
    assert!(
        neighbours.iter().all(|n| n.series_id != seed_id),
        "the seed must never be its own neighbour"
    );

    let best = neighbours[0].series_id;
    assert!(
        dungeon.contains(&best),
        "the closest neighbour of a dungeon series must be a dungeon series, got a romance one"
    );

    let rank_of = |id: SeriesId| neighbours.iter().position(|n| n.series_id == id);
    let worst_dungeon = dungeon[1..]
        .iter()
        .filter_map(|id| rank_of(*id))
        .max()
        .expect("both dungeon siblings must be retrieved");
    let best_romance = romance
        .iter()
        .filter_map(|id| rank_of(*id))
        .min()
        .unwrap_or(usize::MAX);
    assert!(
        worst_dungeon < best_romance,
        "every dungeon series must outrank every romance one; dungeon worst rank {worst_dungeon}, \
         romance best rank {best_romance}"
    );
}

/// An incremental build refuses to run before a full one.
///
/// The bug this pins: projecting with a basis solved from a partial catalogue produces vectors
/// that are not comparable with the stored ones. The index keeps answering — with neighbours
/// that are silently meaningless — which is far worse than an error, so the incremental path
/// must decline rather than improvise a basis.
#[tokio::test]
async fn an_incremental_build_refuses_without_a_basis() {
    let db = TestDb::spawn().await;
    catalogue(&db).await;

    let error = build(&db.pool, budget(), false)
        .await
        .expect_err("an incremental build must not invent a projection");
    assert!(
        error.to_string().contains("full build"),
        "the error must say what to do about it, got: {error}"
    );

    // And the claim must still have been released, or every later build declines forever.
    let state = recsys::read_build_state(&db.pool).await.expect("state");
    assert_eq!(state.stage, "idle", "a failed build must release its claim");
    assert!(state.error.is_some(), "the failure must be recorded");
}

/// A merge removes the absorbed series from the index immediately, and queues the survivor.
///
/// Both halves matter and they are different mechanisms. The loser's rows go with the cascade,
/// in the same transaction that deletes the series — that is what makes a merged id unreachable
/// from the ANN index straight away rather than at the next build. The survivor is *not*
/// automatic: it absorbed the loser's tags and authors, so its digest moved and its embedding is
/// now wrong, which only a re-embed fixes.
#[tokio::test]
async fn a_merge_evicts_the_absorbed_series_and_queues_the_survivor() {
    let db = TestDb::spawn().await;
    let (dungeon, _) = catalogue(&db).await;
    build(&db.pool, budget(), true).await.expect("build");

    let keep = dungeon[0];
    let drop = dungeon[1];
    assert!(
        recsys::embedding_of(&db.pool, drop)
            .await
            .expect("embedding")
            .is_some(),
        "sanity: the series to be absorbed starts out embedded"
    );

    merge_series(&db.pool, keep, drop, None, "merged")
        .await
        .expect("merge");

    assert!(
        recsys::embedding_of(&db.pool, drop)
            .await
            .expect("embedding")
            .is_none(),
        "the absorbed series must leave the index with the merge, not at the next build"
    );

    let queued = recsys::claim_repair_batch(&db.pool, 10)
        .await
        .expect("repair queue");
    assert!(
        queued.contains(&keep),
        "the survivor absorbed the loser's tags and authors, so it must be queued to re-embed"
    );
    assert!(
        !queued.contains(&drop),
        "the absorbed series must not be queued; it no longer exists"
    );

    // An ANN search must never hand back the id that is gone.
    let embedding = recsys::embedding_of(&db.pool, keep)
        .await
        .expect("embedding")
        .expect("the survivor is still embedded");
    let neighbours = recsys::nearest_neighbours(&db.pool, &embedding, keep, 10, 40)
        .await
        .expect("ann search");
    assert!(
        neighbours.iter().all(|n| n.series_id != drop),
        "a merged series must be unreachable from the index"
    );
}

/// The incremental path re-embeds what the repair queue names, under the live generation.
#[tokio::test]
async fn an_incremental_build_drains_the_repair_queue() {
    let db = TestDb::spawn().await;
    let (dungeon, _) = catalogue(&db).await;
    build(&db.pool, budget(), true).await.expect("full build");
    let generation = recsys::read_build_state(&db.pool)
        .await
        .expect("state")
        .generation;

    recsys::enqueue_repair(&db.pool, dungeon[0], "features_changed")
        .await
        .expect("enqueue");
    assert_eq!(recsys::repair_depth(&db.pool).await.expect("depth"), 1);

    let report = build(&db.pool, budget(), false)
        .await
        .expect("incremental build")
        .expect("not claimed elsewhere");
    assert_eq!(
        report.generation, generation,
        "an incremental build patches the live generation rather than taking a new one"
    );
    assert!(report.series_built >= 1, "it must have rebuilt something");
    assert_eq!(
        recsys::repair_depth(&db.pool).await.expect("depth"),
        0,
        "the queue must be drained"
    );

    // Still queryable afterwards: a re-embed must not leave the seed without a vector.
    assert!(
        recsys::embedding_of(&db.pool, dungeon[0])
            .await
            .expect("embedding")
            .is_some()
    );
}

/// A series with almost no metadata must not be recommendable.
///
/// Otherwise the shelf fills with entries nothing is known about, which is indistinguishable
/// from a broken model to anyone looking at it.
#[tokio::test]
async fn a_series_with_too_little_metadata_is_not_recommendable() {
    let db = TestDb::spawn().await;
    let (dungeon, _) = catalogue(&db).await;
    let provider = seed::provider(&db, "beta").create().await;
    let bare = ingest(
        &db,
        provider,
        &Fixture {
            title: "Untitled Scan",
            tags: &[],
            authors: &[],
            content_type: ContentType::Unknown,
        },
    )
    .await;

    build(&db.pool, budget(), true).await.expect("build");

    let top = recsys::top_by_prior(&db.pool, 50).await.expect("prior");
    assert!(
        !top.contains(&bare),
        "a series with no tags, no authors and no medium must not be offered"
    );
    assert!(
        dungeon.iter().any(|id| top.contains(id)),
        "sanity: fully described series are offered"
    );
}
