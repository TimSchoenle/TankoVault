//! `metadata.priority` against a real, migrated schema: which source is allowed to overwrite
//! which field, across the two writers that both touch `series`.
//!
//! The policy itself is unit-tested in `crates/domain/src/metadata_priority.rs`. What is only
//! observable here is whether both write paths actually *consult* it — the bug these pin is a
//! writer that resolved nothing and simply wrote.
//!
//! Gated behind the `integration` feature (requires Docker).
#![cfg(feature = "integration")]

use tankovault_config::MatchingConfig;
use tankovault_db::repo::catalog::{
    MetadataCandidate, MetadataEnrichment, ScannedSeries, SeriesUpsert, apply_enrichment,
    get_series, ingest_series, list_series_titles,
};
use tankovault_domain::{
    ContentType, MetadataPriority, MetadataSource, ProviderId, SeriesId, SeriesStatus,
    normalize_title,
};
use tankovault_test_support::{TestDb, seed};

const ADAPTER_TITLE: &str = "Solo Leveling";
const ADAPTER_BLURB: &str = "Scraped from the provider page.";
const ANILIST_BLURB: &str = "The weakest hunter of all mankind.";

/// One provider scan: what the adapters actually report, including the `Unknown` content type
/// every adapter hardcodes because no site exposes a usable selector for it.
fn scan(provider_id: ProviderId, title: &str, description: &str) -> ScannedSeries {
    ScannedSeries {
        provider_id,
        source_path: "/manga/solo-leveling".to_owned(),
        provider_title: Some(title.to_owned()),
        meta: SeriesUpsert {
            canonical_title: title.to_owned(),
            normalized_title: normalize_title(title),
            description: Some(description.to_owned()),
            cover_url: Some("https://provider.example/cover.jpg".to_owned()),
            content_type: ContentType::Unknown,
            status: SeriesStatus::Unknown,
            release_year: None,
        },
        alt_titles: Vec::new(),
        tags: Vec::new(),
        authors: Vec::new(),
        chapters: Vec::new(),
        content_hash: vec![1],
    }
}

/// One enrichment pass, as `services/sync` builds it from an `AniList` lookup.
fn enrichment<'a>(title: &'a str, description: &'a str) -> MetadataEnrichment<'a> {
    MetadataEnrichment {
        candidate: MetadataCandidate {
            canonical_title: Some(title),
            description: Some(description),
            cover_url: Some("https://anilist.example/cover.jpg"),
            content_type: Some(ContentType::Manhwa),
            status: Some(SeriesStatus::Completed),
            release_year: Some(2018),
        },
        is_adult: Some(false),
        external_score: None,
        external_popularity: None,
        external_source: None,
        alt_titles: &[],
        tags: &[],
        authors: &[],
    }
}

/// The recorded `(description, content_type)` provenance, as `metadata_source` tokens.
///
/// Runtime query rather than `query!`: a read that exists only to inspect the mechanism has no
/// business growing the committed offline cache.
async fn provenance(db: &TestDb, series: SeriesId) -> (Option<String>, Option<String>) {
    sqlx::query_as(
        "SELECT description_source::text, content_type_source::text FROM series WHERE id = $1",
    )
    .bind(series.as_uuid())
    .fetch_one(&db.pool)
    .await
    .expect("read provenance")
}

/// The reported bug: enrichment wrote `AniList`'s description, and the very next catalogue scan
/// wrote the scraped one straight back over it, because ingest resolved no priority at all —
/// it `COALESCE`d, which only protects a stored value from a scan that reports *nothing*.
///
/// The content type is the same bug with the volume up: ingest wrote it unconditionally, so
/// every enriched series reverted to the adapters' hardcoded `unknown` on its next scan and
/// stayed there until the sweep came round again, ~27h later on a 54k-series catalogue.
#[tokio::test]
async fn an_adapter_rescan_does_not_overwrite_enriched_metadata() {
    let db = TestDb::spawn().await;
    let provider = seed::provider(&db, "priority-rescan").create().await;
    let priority = MetadataPriority::default();

    let series = ingest_series(
        &db.pool,
        &scan(provider, ADAPTER_TITLE, ADAPTER_BLURB),
        &MatchingConfig::default(),
        &priority,
        &tankovault_domain::TermBlocklist::default(),
        &tankovault_domain::AdultTagSet::defaults(),
    )
    .await
    .expect("first scan")
    .series_id;

    apply_enrichment(
        &db.pool,
        series,
        &enrichment(ADAPTER_TITLE, ANILIST_BLURB),
        &priority,
        &tankovault_domain::TermBlocklist::default(),
    )
    .await
    .expect("enrich");

    ingest_series(
        &db.pool,
        &scan(provider, ADAPTER_TITLE, ADAPTER_BLURB),
        &MatchingConfig::default(),
        &priority,
        &tankovault_domain::TermBlocklist::default(),
        &tankovault_domain::AdultTagSet::defaults(),
    )
    .await
    .expect("rescan");

    let row = get_series(&db.pool, series).await.expect("read back");
    assert_eq!(
        row.description.as_deref(),
        Some(ANILIST_BLURB),
        "the rescan overwrote an AniList description"
    );
    assert_eq!(
        row.content_type,
        ContentType::Manhwa,
        "the rescan reset the content type to the adapters' hardcoded `unknown`"
    );
    assert_eq!(row.status, SeriesStatus::Completed);
    assert_eq!(row.release_year, Some(2018));
    assert_eq!(
        provenance(&db, series).await,
        (Some("anilist".to_owned()), Some("anilist".to_owned()))
    );
}

/// The other half of the same bug: the priority is configuration, so an operator naming the
/// adapters first must get the scraped value — otherwise "`AniList` wins" is hardcoded and the
/// config is decoration.
#[tokio::test]
async fn an_adapter_first_order_lets_the_scan_win() {
    let db = TestDb::spawn().await;
    let provider = seed::provider(&db, "priority-adapter-first").create().await;
    let priority = MetadataPriority {
        description: vec![MetadataSource::Adapter, MetadataSource::AniList],
        ..MetadataPriority::default()
    };

    let series = ingest_series(
        &db.pool,
        &scan(provider, ADAPTER_TITLE, ADAPTER_BLURB),
        &MatchingConfig::default(),
        &priority,
        &tankovault_domain::TermBlocklist::default(),
        &tankovault_domain::AdultTagSet::defaults(),
    )
    .await
    .expect("first scan")
    .series_id;

    apply_enrichment(
        &db.pool,
        series,
        &enrichment(ADAPTER_TITLE, ANILIST_BLURB),
        &priority,
        &tankovault_domain::TermBlocklist::default(),
    )
    .await
    .expect("enrich");

    let row = get_series(&db.pool, series).await.expect("read back");
    assert_eq!(
        row.description.as_deref(),
        Some(ADAPTER_BLURB),
        "enrichment ignored an adapter-first description order"
    );
    // Only `description` was re-ordered; every other field still follows the default.
    assert_eq!(row.content_type, ContentType::Manhwa);
    assert_eq!(
        provenance(&db, series).await,
        (Some("adapter".to_owned()), Some("anilist".to_owned()))
    );
}

/// A title that loses the priority contest is still a name the work is published under, and
/// `series_titles` is what a later cross-provider scan looks the series up by. Dropping the
/// loser would make winning the title contest quietly cost the catalogue a matching key.
#[tokio::test]
async fn a_losing_title_is_kept_as_an_alternative() {
    let db = TestDb::spawn().await;
    let provider = seed::provider(&db, "priority-titles").create().await;
    let priority = MetadataPriority::default();
    let romaji = "Na Honjaman Level Up";

    let series = ingest_series(
        &db.pool,
        &scan(provider, ADAPTER_TITLE, ADAPTER_BLURB),
        &MatchingConfig::default(),
        &priority,
        &tankovault_domain::TermBlocklist::default(),
        &tankovault_domain::AdultTagSet::defaults(),
    )
    .await
    .expect("first scan")
    .series_id;

    apply_enrichment(
        &db.pool,
        series,
        &enrichment(romaji, ANILIST_BLURB),
        &priority,
        &tankovault_domain::TermBlocklist::default(),
    )
    .await
    .expect("enrich");

    let row = get_series(&db.pool, series).await.expect("read back");
    assert_eq!(row.canonical_title, romaji);
    assert_eq!(
        row.normalized_title,
        normalize_title(romaji),
        "the normalized title must follow the canonical one, or trigram lookup searches for a \
         title the series no longer has"
    );

    let titles = list_series_titles(&db.pool, series).await.expect("titles");
    assert!(
        titles.iter().any(|t| t == ADAPTER_TITLE),
        "the displaced provider title was dropped instead of kept as an alternative: {titles:?}"
    );
}
