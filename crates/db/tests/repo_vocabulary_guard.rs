//! The intake vocabulary guard against a real, migrated schema: which scraped terms are allowed
//! to become rows in the two shared vocabularies, across the two writers that both fill them.
//!
//! The guard itself is unit-tested in `crates/domain/src/term_filter.rs`. What is only observable
//! here is whether each writer actually *consults* it — the bug these pin is a writer that had no
//! guard at all.
//!
//! Gated behind the `integration` feature (requires Docker).
#![cfg(feature = "integration")]

use tankovault_config::MatchingConfig;
use tankovault_db::repo::catalog::{
    MetadataCandidate, MetadataEnrichment, ScannedSeries, SeriesUpsert, TagLink, apply_enrichment,
    ingest_series,
};
use tankovault_domain::{
    AdultTagSet, ContentType, MetadataPriority, ProviderId, SeriesId, SeriesStatus, TermBlocklist,
    normalize_title,
};
use tankovault_test_support::{TestDb, seed};

const TITLE: &str = "Omniscient Reader";
const CREATOR: &str = "Sing Shong";

/// One provider scan of a Madara-style summary block: the genre row and the credit row of the
/// same template, placeholders and all.
fn scan(provider_id: ProviderId) -> ScannedSeries {
    ScannedSeries {
        provider_id,
        source_path: "/manga/omniscient-reader".to_owned(),
        provider_title: Some(TITLE.to_owned()),
        meta: SeriesUpsert {
            canonical_title: TITLE.to_owned(),
            normalized_title: normalize_title(TITLE),
            description: None,
            cover_url: None,
            content_type: ContentType::Unknown,
            status: SeriesStatus::Unknown,
            release_year: None,
        },
        alt_titles: Vec::new(),
        tags: vec!["Action".to_owned(), "Updating".to_owned()],
        authors: vec![CREATOR.to_owned(), "Updating".to_owned()],
        chapters: Vec::new(),
        content_hash: vec![1],
    }
}

/// The display names actually linked to a series, per vocabulary.
///
/// Runtime queries rather than `query!`: a read that exists only to inspect the mechanism has no
/// business growing the committed offline cache.
async fn credits(db: &TestDb, series: SeriesId) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT a.name FROM series_authors sa JOIN authors a ON a.id = sa.author_id \
         WHERE sa.series_id = $1 ORDER BY a.name",
    )
    .bind(series.as_uuid())
    .fetch_all(&db.pool)
    .await
    .expect("read credits")
}

async fn genres(db: &TestDb, series: SeriesId) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT t.name FROM series_tags st JOIN tags t ON t.id = st.tag_id \
         WHERE st.series_id = $1 ORDER BY t.name",
    )
    .bind(series.as_uuid())
    .fetch_all(&db.pool)
    .await
    .expect("read genres")
}

/// The reported bug: a taste profile whose strongest term was `Updating`.
///
/// The guard was applied where a tag is interned and nowhere else, so the `Author: Updating` row
/// — rendered by the same template, directly under `Genres: Updating` — reached `authors`
/// unfiltered. Author is the recommender's heaviest axis and its one *exact* retrieval path, so a
/// placeholder credit shared by a large part of the catalogue went to the top of every profile
/// and pulled unrelated series onto every shelf. Discover looked clean throughout, because
/// Discover renders tags.
#[tokio::test]
async fn a_refused_term_becomes_neither_a_tag_nor_a_credit_on_a_scan() {
    let db = TestDb::spawn().await;
    let provider = seed::provider(&db, "vocabulary-guard-scan").create().await;

    let series = ingest_series(
        &db.pool,
        &scan(provider),
        &MatchingConfig::default(),
        &MetadataPriority::default(),
        &TermBlocklist::defaults(),
        &AdultTagSet::defaults(),
    )
    .await
    .expect("scan")
    .series_id;

    assert_eq!(
        credits(&db, series).await,
        vec![CREATOR.to_owned()],
        "the placeholder credit survived intake"
    );
    assert_eq!(
        genres(&db, series).await,
        vec!["Action".to_owned()],
        "the placeholder genre survived intake"
    );
}

/// The same guard, at the other writer. `services/sync` folds `AniList`'s staff list in through
/// [`apply_enrichment`], and a rule only one of the two writers consults is not a rule.
#[tokio::test]
async fn a_refused_term_becomes_neither_a_tag_nor_a_credit_on_an_enrichment() {
    let db = TestDb::spawn().await;
    let provider = seed::provider(&db, "vocabulary-guard-enrich").create().await;

    let mut clean = scan(provider);
    clean.tags.clear();
    clean.authors.clear();
    let series = ingest_series(
        &db.pool,
        &clean,
        &MatchingConfig::default(),
        &MetadataPriority::default(),
        &TermBlocklist::defaults(),
        &AdultTagSet::defaults(),
    )
    .await
    .expect("scan")
    .series_id;

    let authors = vec![CREATOR.to_owned(), "N/A".to_owned()];
    let tags = [TagLink::genre("Action"), TagLink::genre("N/A")];
    apply_enrichment(
        &db.pool,
        series,
        &MetadataEnrichment {
            candidate: MetadataCandidate::default(),
            is_adult: None,
            external_score: None,
            external_popularity: None,
            external_source: None,
            alt_titles: &[],
            tags: &tags,
            authors: &authors,
        },
        &MetadataPriority::default(),
        &TermBlocklist::defaults(),
    )
    .await
    .expect("enrich");

    assert_eq!(
        credits(&db, series).await,
        vec![CREATOR.to_owned()],
        "the placeholder credit survived enrichment"
    );
    assert_eq!(
        genres(&db, series).await,
        vec!["Action".to_owned()],
        "the placeholder genre survived enrichment"
    );
}

/// An operator who switches the guard off must get what their catalogue actually publishes, in
/// both vocabularies — the empty list is the guard disabled, not a guard that blocks everything.
#[tokio::test]
async fn an_empty_guard_admits_the_terms_it_would_otherwise_refuse() {
    let db = TestDb::spawn().await;
    let provider = seed::provider(&db, "vocabulary-guard-off").create().await;

    let series = ingest_series(
        &db.pool,
        &scan(provider),
        &MatchingConfig::default(),
        &MetadataPriority::default(),
        &TermBlocklist::default(),
        &AdultTagSet::defaults(),
    )
    .await
    .expect("scan")
    .series_id;

    assert_eq!(
        credits(&db, series).await,
        vec![CREATOR.to_owned(), "Updating".to_owned()],
        "an empty guard must not filter credits"
    );
}
