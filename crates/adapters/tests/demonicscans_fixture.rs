//! Fixture tests for the demonicscans.org custom adapter, using trimmed live markup so a
//! provider layout change breaks a test, not production. No network is touched.

use async_trait::async_trait;
use std::sync::Arc;
use tankovault_adapters::{Ctx, DemonicScansAdapter, SourceAdapter};
use tankovault_domain::SeriesStatus;
use tankovault_fetch::{FetchError, FetchRequest, FetchResponse, Fetcher};

const CATALOG_HTML: &str = include_str!("../fixtures/demonicscans/catalog.html");
const LATEST_HTML: &str = include_str!("../fixtures/demonicscans/latest.html");
const SERIES_HTML: &str = include_str!("../fixtures/demonicscans/series.html");

struct FixtureFetcher;

#[async_trait]
impl Fetcher for FixtureFetcher {
    async fn get(&self, req: FetchRequest) -> Result<FetchResponse, FetchError> {
        let body = if req.url.contains("/advanced.php") {
            CATALOG_HTML
        } else if req.url.contains("/manga/") {
            // Series metadata and chapters both live on the /manga/<slug> page.
            SERIES_HTML
        } else {
            // Bare domain root = the "latest updates" home feed.
            LATEST_HTML
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
        base_url: "https://demonicscans.org".to_owned(),
        provider_slug: "demonicscans".to_owned(),
        fetcher: Arc::new(FixtureFetcher),
    }
}

fn adapter() -> DemonicScansAdapter {
    DemonicScansAdapter::new()
}

#[tokio::test]
async fn parses_catalog_with_next_marker() {
    let page = adapter()
        .list_catalog(&ctx(), 1)
        .await
        .expect("catalog parses");

    assert_eq!(page.items.len(), 3);
    assert!(page.has_next, "explicit Next anchor marks another page");
    // The full title comes from the anchor's `title` attr, not the truncated <h1>.
    assert_eq!(page.items[0].title, "Gangho, Such Madness!");
    assert!(page.items.iter().all(|i| i.path.starts_with("/manga/")));
    assert!(page.items.iter().any(|i| i.path == "/manga/Shibuya-Noir"));
}

/// Regression: the home feed lists the site's text novels alongside its comics, at `/novel/`.
/// The catalogue never yields that prefix and the site answers those pages with a 404, so every
/// one ingested became a series whose every rescan spent its retry budget failing — four of them
/// accounted for more than a thousand recorded failures before anyone looked.
#[tokio::test]
async fn parses_latest_feed() {
    let updates = adapter().list_latest(&ctx()).await.expect("latest parses");

    assert_eq!(updates.len(), 2, "the novel card is not a series");
    assert!(
        updates.iter().all(|u| u.path.starts_with("/manga/")),
        "{:?}",
        updates.iter().map(|u| &u.path).collect::<Vec<_>>()
    );
    assert_eq!(updates[0].title, "Welcome to Dungeon Hotel");
    assert_eq!(updates[0].path, "/manga/Welcome-to-Dungeon-Hotel");
    // Newest chapter number read from the first chapter link.
    assert!((updates[0].latest_chapter - 97.0).abs() < 1e-9);
}

#[tokio::test]
async fn parses_series_metadata() {
    let meta = adapter()
        .fetch_series(&ctx(), "/manga/Shibuya-Noir")
        .await
        .expect("series parses");

    assert_eq!(meta.title, "Shibuya Noir");
    assert_eq!(meta.status, SeriesStatus::Ongoing);
    assert!(meta.tags.iter().any(|t| t == "Seinen"));
    assert!(meta.tags.iter().any(|t| t == "Action"));
    // Author cell reuses the same "," split as Alternatives.
    assert_eq!(
        meta.authors,
        vec!["Nemeton".to_owned(), "Mangamuse".to_owned()]
    );
    // Description keeps only the synopsis after the "The Summary is" boilerplate marker.
    let desc = meta.description.expect("description present");
    assert!(desc.starts_with("Yuto Kanzaki"));
    assert!(!desc.contains("The Summary is"));
    // Cover is an absolute CDN URL; the space in the filename is percent-encoded so the
    // stored value is a valid, directly-fetchable URL.
    assert_eq!(
        meta.cover_url.as_deref(),
        Some("https://readermc.org/images/thumbnails/Shibuya%20Noir.webp")
    );
    // This series lists no alternatives (the cell is &nbsp;).
    assert!(meta.alt_titles.is_empty());
}

#[tokio::test]
async fn parses_chapters_with_dates() {
    let chapters = adapter()
        .fetch_chapters(&ctx(), "/manga/Shibuya-Noir")
        .await
        .expect("chapters parse");

    assert_eq!(chapters.len(), 4);
    // Decimal chapters are supported; the trailing ISO date is not mistaken for a number.
    assert!(chapters.iter().any(|c| (c.number - 15.5).abs() < 1e-9));
    assert!(chapters.iter().any(|c| (c.number - 17.0).abs() < 1e-9));
    // Chapter links are stored relative.
    assert!(
        chapters
            .iter()
            .all(|c| c.path.starts_with("/chaptered.php?"))
    );
    // The ISO release date is parsed; the empty date cell leaves it unset.
    let ch17 = chapters
        .iter()
        .find(|c| (c.number - 17.0).abs() < 1e-9)
        .unwrap();
    assert_eq!(ch17.published_at.expect("ch17 has a date").year(), 2026);
    let ch1 = chapters
        .iter()
        .find(|c| (c.number - 1.0).abs() < 1e-9)
        .unwrap();
    assert!(ch1.published_at.is_none());
}
