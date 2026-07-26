//! Fixture tests for the shipped provider presets. `manhuaus` is a straight Madara preset;
//! `kunmanga` is the custom hybrid (Madara-shaped catalogue/series HTML, JSON chapter API).
//! These exercise the *actual* `presets::builtin()` config against markup/JSON trimmed from
//! live solver-fetched responses, so a wrong selector or a chapter-API regression fails here
//! rather than in production.

use async_trait::async_trait;
use std::sync::Arc;
use tankovault_adapters::{Ctx, SourceAdapter, build_adapter, builtin_presets};
use tankovault_domain::SeriesStatus;
use tankovault_fetch::{FetchError, FetchRequest, FetchResponse, Fetcher};

/// Serves fixture bodies by URL shape. Every field defaults to an empty body, so each test
/// supplies only the documents the call under test actually fetches.
#[derive(Default)]
struct SiteFetcher {
    catalog: &'static str,
    series: &'static str,
    /// Body served for the kunmanga JSON chapter API (`/api/comics/…`); unused elsewhere.
    chapters_api: &'static str,
    /// Bodies for kunmanga's sitemap index and its series shards; unused elsewhere.
    sitemap_index: &'static str,
    sitemap_shard: &'static str,
}

#[async_trait]
impl Fetcher for SiteFetcher {
    async fn get(&self, req: FetchRequest) -> Result<FetchResponse, FetchError> {
        // Route by URL shape: sitemap documents, the JSON chapter API, catalogue listings
        // under `/manga/page/`, else a series page.
        let body = if req.url.contains("/sitemap-comic-") {
            self.sitemap_shard
        } else if req.url.ends_with("/sitemap.xml") {
            self.sitemap_index
        } else if req.url.contains("/api/comics/") {
            self.chapters_api
        } else if req.url.contains("/manga/page/") {
            self.catalog
        } else {
            self.series
        };
        Ok(FetchResponse {
            status: 200,
            url: req.url.clone(),
            headers: Vec::new(),
            body: body.to_owned(),
            from_cache: false,
        })
    }
}

/// Build the live adapter for a shipped preset, paired with a fixture-serving context.
fn preset_adapter(slug: &str, fetcher: SiteFetcher) -> (Box<dyn SourceAdapter>, Ctx) {
    let preset = builtin_presets()
        .into_iter()
        .find(|p| p.slug == slug)
        .expect("preset exists");
    let adapter = build_adapter(preset.adapter, preset.slug, &preset.config)
        .expect("preset builds an adapter");
    let ctx = Ctx {
        base_url: preset.base_url.to_owned(),
        provider_slug: preset.slug.to_owned(),
        fetcher: Arc::new(fetcher),
    };
    (adapter, ctx)
}

const MANHUAUS_CATALOG: &str = include_str!("../fixtures/manhuaus/catalog.html");
const MANHUAUS_SERIES: &str = include_str!("../fixtures/manhuaus/series.html");
const KUNMANGA_CATALOG: &str = include_str!("../fixtures/kunmanga/catalog.html");
const KUNMANGA_SERIES: &str = include_str!("../fixtures/kunmanga/series.html");
const KUNMANGA_CHAPTERS_API: &str = include_str!("../fixtures/kunmanga/chapters.json");
const KUNMANGA_SITEMAP_INDEX: &str = include_str!("../fixtures/kunmanga/sitemap-index.xml");
const KUNMANGA_SITEMAP_SHARD: &str = include_str!("../fixtures/kunmanga/sitemap-comic.xml");

/// The fixture set for kunmanga: every document any of its adapter calls can reach.
fn kunmanga_fixtures() -> SiteFetcher {
    SiteFetcher {
        catalog: KUNMANGA_CATALOG,
        series: KUNMANGA_SERIES,
        chapters_api: KUNMANGA_CHAPTERS_API,
        sitemap_index: KUNMANGA_SITEMAP_INDEX,
        sitemap_shard: KUNMANGA_SITEMAP_SHARD,
    }
}

#[tokio::test]
async fn manhuaus_catalog_uses_head_rel_next() {
    let (adapter, ctx) = preset_adapter(
        "manhuaus",
        SiteFetcher {
            catalog: MANHUAUS_CATALOG,
            series: MANHUAUS_SERIES,
            ..SiteFetcher::default()
        },
    );
    let page = adapter.list_catalog(&ctx, 1).await.expect("catalog parses");

    assert_eq!(page.items.len(), 2);
    assert!(page.has_next, "<link rel=next> marks another page");
    assert_eq!(page.items[0].title, "Reborn As The Heavenly Demon");
    assert_eq!(
        page.items[0].path,
        "/manga/reincarnation-of-the-heavenly-demon/"
    );
}

#[tokio::test]
async fn manhuaus_series_reads_lazy_cover() {
    let (adapter, ctx) = preset_adapter(
        "manhuaus",
        SiteFetcher {
            catalog: MANHUAUS_CATALOG,
            series: MANHUAUS_SERIES,
            ..SiteFetcher::default()
        },
    );
    let meta = adapter
        .fetch_series(&ctx, "/manga/reincarnation-of-the-heavenly-demon/")
        .await
        .expect("series parses");

    assert_eq!(meta.title, "Reborn As The Heavenly Demon");
    assert_eq!(meta.status, SeriesStatus::Ongoing);
    assert!(meta.tags.iter().any(|t| t == "Action"));
    // The override reads the real cover from data-src, not the base64 src placeholder.
    assert_eq!(
        meta.cover_url.as_deref(),
        Some("https://manhuaus.com/wp-content/uploads/2024/03/heavenly-demon-193x278.webp")
    );

    let chapters = adapter
        .fetch_chapters(&ctx, "/manga/reincarnation-of-the-heavenly-demon/")
        .await
        .expect("chapters parse");
    assert_eq!(chapters.len(), 3);
    assert!(chapters.iter().any(|c| (c.number - 131.0).abs() < 1e-9));
    assert!(chapters.iter().all(|c| c.path.starts_with("/manga/")));
}

#[tokio::test]
async fn kunmanga_catalog_reads_series_from_sitemap_shards() {
    let (adapter, ctx) = preset_adapter("kunmanga", kunmanga_fixtures());
    let page = adapter.list_catalog(&ctx, 1).await.expect("catalog parses");

    // Entries come from the sitemap shard, as relative series paths.
    assert_eq!(page.items.len(), 5);
    assert!(page.items.iter().all(|i| i.path.starts_with("/manga/")));
    assert_eq!(page.items[0].path, "/manga/moonchild");
    // The sitemap carries no titles, so the stub title is derived from the slug.
    assert_eq!(page.items[0].title, "Moonchild");

    // The index lists 5 comic shards, so page 1 has more to come...
    assert!(page.has_next, "more series shards remain after page 1");
}

#[tokio::test]
async fn kunmanga_catalog_terminates_after_the_last_shard() {
    let (adapter, ctx) = preset_adapter("kunmanga", kunmanga_fixtures());

    // ...and the walk stops on the index's own shard count rather than on a heuristic —
    // the fixture index lists 5 comic shards (chapter shards and `sitemap0` are ignored).
    let last = adapter.list_catalog(&ctx, 5).await.expect("catalog parses");
    assert!(!last.has_next, "page 5 is the final series shard");

    let past_end = adapter.list_catalog(&ctx, 6).await.expect("catalog parses");
    assert!(past_end.items.is_empty());
    assert!(!past_end.has_next);
}

#[tokio::test]
async fn kunmanga_series_metadata_from_html() {
    let (adapter, ctx) = preset_adapter("kunmanga", kunmanga_fixtures());
    let meta = adapter
        .fetch_series(&ctx, "/manga/its-just-business")
        .await
        .expect("series parses");

    // Catalogue/series metadata still comes from the Madara-shaped HTML.
    assert_eq!(meta.title, "It's Just Business");
    assert_eq!(meta.status, SeriesStatus::Completed);
    assert!(meta.tags.iter().any(|t| t == "Romance"));
    // The `release` override reads the year from the manga-release archive link.
    assert_eq!(meta.release_year, Some(2025));
    // kunmanga serves the real cover URL directly on src.
    assert_eq!(
        meta.cover_url.as_deref(),
        Some("https://cdn.zinmanga1.com/thumb/its-just-business.webp")
    );
}

#[tokio::test]
async fn kunmanga_chapters_from_json_api() {
    let (adapter, ctx) = preset_adapter("kunmanga", kunmanga_fixtures());

    // Chapters are NOT in the series HTML — they come from `/api/comics/{slug}/chapters`.
    let chapters = adapter
        .fetch_chapters(&ctx, "/manga/its-just-business")
        .await
        .expect("chapters parse from the JSON API");
    assert_eq!(chapters.len(), 3);
    let ch118 = chapters
        .iter()
        .find(|c| (c.number - 118.0).abs() < 1e-9)
        .expect("chapter 118 present");
    assert_eq!(ch118.path, "/manga/its-just-business/chapter-118");
    assert!(
        ch118.published_at.is_some(),
        "updated_at parsed into a date"
    );
}
