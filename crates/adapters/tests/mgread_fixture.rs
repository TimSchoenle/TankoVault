//! Fixture tests for the `mgread` adapter, against the *shipped* preset config.
//!
//! Every fixture is a live response trimmed to what the adapter reads. The site's chapter list
//! is the interesting part: the page carries only its newest 24 rows, so the assertions below
//! pin that the walk goes to the REST endpoint and follows its `total_pages`.

use async_trait::async_trait;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tankovault_adapters::{Ctx, SourceAdapter, build_adapter, builtin_presets};
use tankovault_fetch::{FetchError, FetchRequest, FetchResponse, Fetcher};

const CATALOG: &str = include_str!("../fixtures/mgread/catalog.html");
const CATALOG_LAST: &str = include_str!("../fixtures/mgread/catalog-last.html");
const LATEST: &str = include_str!("../fixtures/mgread/latest.html");
const SERIES: &str = include_str!("../fixtures/mgread/series.html");
const CHAPTERS: &str = include_str!("../fixtures/mgread/chapters.json");
const CHAPTERS_PAGE_2: &str = include_str!("../fixtures/mgread/chapters-page2.json");
const POST: &str = include_str!("../fixtures/mgread/post.json");

/// Serves fixture bodies by URL shape, counting what each call costs.
///
/// `post` empty is how a deployment with the `WordPress` core routes locked down is expressed:
/// the body fails to deserialize and the adapter falls back to the series page.
#[derive(Default)]
struct SiteFetcher {
    catalog: &'static str,
    latest: &'static str,
    series: &'static str,
    post: &'static str,
    chapters: &'static str,
    chapters_page_2: &'static str,
    chapter_api_calls: AtomicUsize,
    series_page_fetches: AtomicUsize,
}

#[async_trait]
impl Fetcher for SiteFetcher {
    async fn get(&self, req: FetchRequest) -> Result<FetchResponse, FetchError> {
        let body = if req.url.contains("/wp-json/wp/v2/manga") {
            self.post
        } else if req.url.contains("/wp-json/initmanga/") {
            self.chapter_api_calls.fetch_add(1, Ordering::Relaxed);
            if req.url.contains("paged=1") {
                self.chapters
            } else {
                self.chapters_page_2
            }
        } else if req.url.contains("/recently-updated/") {
            self.latest
        } else if req.url.contains("/manga/page/") {
            self.catalog
        } else {
            self.series_page_fetches.fetch_add(1, Ordering::Relaxed);
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

/// Build the live adapter for the shipped preset, paired with a fixture-serving context.
fn preset_adapter(fetcher: Arc<SiteFetcher>) -> (Box<dyn SourceAdapter>, Ctx) {
    let preset = builtin_presets()
        .into_iter()
        .find(|p| p.slug == "mgread")
        .expect("no shipped preset named mgread");
    let adapter = build_adapter(preset.adapter, preset.slug, &preset.config)
        .expect("mgread preset failed to build");
    let ctx = Ctx {
        base_url: preset.base_url.to_owned(),
        provider_slug: preset.slug.to_owned(),
        fetcher,
    };
    (adapter, ctx)
}

#[tokio::test]
async fn the_catalogue_reads_cards_and_chains_on_the_head_link() {
    let (adapter, ctx) = preset_adapter(Arc::new(SiteFetcher {
        catalog: CATALOG,
        ..SiteFetcher::default()
    }));

    let page = adapter.list_catalog(&ctx, 1).await.expect("catalog parses");

    assert_eq!(page.items.len(), 2);
    assert_eq!(page.items[0].title, "God of Wine (Seong-Hye)");
    assert_eq!(page.items[0].path, "/manga/god-of-wine-seong-hye/");
    assert!(
        page.has_next,
        "`link[rel=next]` is in the head of this page"
    );
}

/// The theme's visible paginator renders a "Next page" control on the last listing too, and the
/// page after it answers 404 — so a walk that waited for an empty page would end on an error
/// rather than a signal. `link[rel=next]` is absent here, which is what stops it.
#[tokio::test]
async fn the_last_catalogue_page_ends_the_walk_while_still_yielding_items() {
    let (adapter, ctx) = preset_adapter(Arc::new(SiteFetcher {
        catalog: CATALOG_LAST,
        ..SiteFetcher::default()
    }));

    let page = adapter
        .list_catalog(&ctx, 266)
        .await
        .expect("catalog parses");

    assert!(
        !page.items.is_empty(),
        "the fixture must yield items, or this proves nothing"
    );
    assert!(!page.has_next);
}

#[tokio::test]
async fn the_feed_reads_the_newest_chapter_off_each_card() {
    let (adapter, ctx) = preset_adapter(Arc::new(SiteFetcher {
        latest: LATEST,
        ..SiteFetcher::default()
    }));

    let updates = adapter.list_latest(&ctx).await.expect("feed parses");

    assert_eq!(updates.len(), 2);
    assert_eq!(updates[0].path, "/manga/dungeon-porter/");
    assert_eq!(updates[0].title, "Dungeon Porter");
    assert!(
        (updates[0].latest_chapter - 73.0).abs() < f64::EPSILON,
        "the first of the card's two chapter buttons is the newest"
    );
}

/// The reading-time pill (`1h 12m to finish`) sits in `#genre-tags` wearing the genres' classes
/// and would otherwise be interned as a tag — one distinct facet per duration, and a feature the
/// recommender would learn from.
#[tokio::test]
async fn series_metadata_takes_the_genres_and_not_the_reading_time_pill() {
    let (adapter, ctx) = preset_adapter(Arc::new(SiteFetcher {
        series: SERIES,
        ..SiteFetcher::default()
    }));

    let meta = adapter
        .fetch_series(&ctx, "/manga/a-siblings-pov/")
        .await
        .expect("series parses");

    assert_eq!(meta.title, "A Sibling’s POV");
    assert_eq!(
        meta.tags,
        vec!["Comedy".to_owned(), "Supernatural".to_owned()]
    );
    assert_eq!(
        meta.alt_titles,
        vec![
            "First-Person Blood-Relative Perspective".to_owned(),
            "1인칭 혈육시점".to_owned(),
            "A Sibling's Point Of View".to_owned(),
        ],
        "the alternate-title cell lists them in one node, `;`-separated"
    );
    assert!(meta.cover_url.is_some());
    assert!(meta.description.is_some());
}

/// Bug guard for the failure this adapter exists to prevent: the series page server-renders only
/// its newest 24 chapter rows, so reading chapters from markup truncates a long series to its
/// most recent page — no error, just a chapter count that looks plausible and is wrong.
#[tokio::test]
async fn chapters_come_from_the_endpoint_and_follow_its_page_count() {
    let fetcher = Arc::new(SiteFetcher {
        post: POST,
        chapters: CHAPTERS,
        chapters_page_2: CHAPTERS_PAGE_2,
        ..SiteFetcher::default()
    });
    let (adapter, ctx) = preset_adapter(Arc::clone(&fetcher));

    let chapters = adapter
        .fetch_chapters(&ctx, "/manga/a-siblings-pov/")
        .await
        .expect("chapters parse");

    assert_eq!(
        fetcher.chapter_api_calls.load(Ordering::Relaxed),
        2,
        "`total_pages` says two, and both have to be walked"
    );
    assert_eq!(
        fetcher.series_page_fetches.load(Ordering::Relaxed),
        0,
        "the 130 KB page is not fetched again for facts the posts route answers in a line"
    );
    let numbers: Vec<f64> = chapters.iter().map(|c| c.number).collect();
    assert_eq!(
        numbers,
        vec![18.0, 17.0, 46.1],
        "the null-numbered `chapter-46-1` row is recovered from its slug, and `prologue` — \
         which carries no number anywhere — is dropped rather than guessed"
    );
    assert_eq!(
        chapters[0].path, "/manga/a-siblings-pov/chapter-18/",
        "the reader link is the series path plus the chapter slug"
    );
}

/// The chapter endpoint serves `WordPress`'s stored *site-local* time with no offset on it, so
/// every release on this site would land seven hours out if it were read as UTC. The posts route
/// states the offset as the gap between its `date` and `date_gmt`.
#[tokio::test]
async fn a_chapter_date_carries_the_sites_own_offset() {
    let (adapter, ctx) = preset_adapter(Arc::new(SiteFetcher {
        post: POST,
        chapters: CHAPTERS,
        chapters_page_2: CHAPTERS_PAGE_2,
        ..SiteFetcher::default()
    }));

    let chapters = adapter
        .fetch_chapters(&ctx, "/manga/a-siblings-pov/")
        .await
        .expect("chapters parse");

    let published = chapters[0].published_at.expect("row carries a date");
    assert_eq!(
        published,
        time::macros::datetime!(2026-08-09 07:12:06 +7),
        "`2026-08-09 07:12:06` from the chapter endpoint, `+07:00` from the posts route"
    );
}

/// A deployment with the `WordPress` core routes locked down still has to work: the series page
/// states both facts too, and the adapter falls back to it rather than losing the provider. The
/// offset then comes from a chapter row's own RFC 3339 timestamp.
#[tokio::test]
async fn an_unavailable_posts_route_falls_back_to_the_series_page() {
    let fetcher = Arc::new(SiteFetcher {
        series: SERIES,
        chapters: CHAPTERS,
        chapters_page_2: CHAPTERS_PAGE_2,
        // `post` left empty: the route answers with something that is not a post list.
        ..SiteFetcher::default()
    });
    let (adapter, ctx) = preset_adapter(Arc::clone(&fetcher));

    let chapters = adapter
        .fetch_chapters(&ctx, "/manga/a-siblings-pov/")
        .await
        .expect("chapters parse");

    assert_eq!(fetcher.series_page_fetches.load(Ordering::Relaxed), 1);
    assert_eq!(
        chapters[0].published_at.expect("row carries a date"),
        time::macros::datetime!(2026-08-09 07:12:06 +7),
        "the page's own chapter rows carry the same offset"
    );
}
