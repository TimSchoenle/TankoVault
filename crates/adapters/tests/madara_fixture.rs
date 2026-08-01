//! Fixture tests for the Madara config-driven adapter, using checked-in HTML so a provider
//! markup change breaks a test, not production. No network is touched.

use async_trait::async_trait;
use std::sync::Arc;
use tankovault_adapters::{Ctx, build_adapter};
use tankovault_domain::{AdapterKind, SeriesStatus};
use tankovault_fetch::{FetchError, FetchRequest, FetchResponse, Fetcher};

const CATALOG_HTML: &str = include_str!("../fixtures/madara-sample/catalog.html");
const SERIES_HTML: &str = include_str!("../fixtures/madara-sample/series.html");

struct FixtureFetcher;

#[async_trait]
impl Fetcher for FixtureFetcher {
    async fn get(&self, req: FetchRequest) -> Result<FetchResponse, FetchError> {
        // Catalogue listing pages carry `/manga/?page=`; everything else is a series page.
        let body = if req.url.contains("/manga/?page=") {
            CATALOG_HTML
        } else {
            SERIES_HTML
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

fn ctx() -> Ctx {
    Ctx {
        base_url: "https://madara.test".to_owned(),
        provider_slug: "madara-sample".to_owned(),
        fetcher: Arc::new(FixtureFetcher),
    }
}

fn madara_adapter() -> Box<dyn tankovault_adapters::SourceAdapter> {
    build_adapter(AdapterKind::Madara, "madara-sample", &serde_json::json!({}))
        .expect("madara adapter builds from defaults")
}

#[tokio::test]
async fn parses_series_metadata() {
    let adapter = madara_adapter();
    let meta = adapter
        .fetch_series(&ctx(), "/manga/solo-leveling/")
        .await
        .expect("series parses");

    assert_eq!(meta.title, "Solo Leveling");
    assert_eq!(meta.status, SeriesStatus::Ongoing);
    assert!(meta.tags.iter().any(|t| t == "Action"));
    assert!(meta.tags.iter().any(|t| t == "Fantasy"));
    // Bug this pins: `alt` was the plain selector `div.summary-heading`, which matched every
    // row's label, mislabelling Author(s)/Genre(s)/Status into `series_titles` too.
    assert_eq!(
        meta.alt_titles,
        vec!["Only I Level Up".to_owned(), "나 혼자만 레벨업".to_owned()]
    );
    assert!(meta.description.unwrap().contains("Ten years ago"));
    // Cover is a link only, resolved to an absolute URL for direct client use.
    assert_eq!(
        meta.cover_url.as_deref(),
        Some("https://cdn.madara.test/covers/solo-leveling.jpg")
    );
}

#[tokio::test]
async fn parses_chapter_list_with_relative_paths() {
    let adapter = madara_adapter();
    let chapters = adapter
        .fetch_chapters(&ctx(), "/manga/solo-leveling/")
        .await
        .expect("chapters parse");

    assert_eq!(chapters.len(), 4);
    // Decimal chapter numbers are supported.
    assert!(chapters.iter().any(|c| (c.number - 10.5).abs() < 1e-9));
    // Every stored path is RELATIVE — even the one that was absolute in the markup.
    assert!(chapters.iter().all(|c| c.path.starts_with('/')));
    assert!(
        chapters
            .iter()
            .any(|c| c.path == "/manga/solo-leveling/chapter-10/")
    );
}

#[tokio::test]
async fn parses_catalog_page() {
    let adapter = madara_adapter();
    let page = adapter
        .list_catalog(&ctx(), 1)
        .await
        .expect("catalog parses");

    assert_eq!(page.items.len(), 3);
    assert!(page.has_next); // a.nextpostslink is present
    assert_eq!(page.items[0].title, "Solo Leveling");
    assert!(page.items[0].path.starts_with("/manga/"));
}
