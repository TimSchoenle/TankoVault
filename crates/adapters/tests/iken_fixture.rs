//! Fixture tests for the Iken platform adapter, against the *shipped* preset config.
//!
//! Every fixture is a live response trimmed to the fields the adapter reads. The platform
//! publishes its paywall as data (`isLocked`/`unlockAt`), so the early-access assertions here
//! pin an exact contract rather than an inferred one.

use async_trait::async_trait;
use std::sync::Arc;
use tankovault_adapters::{ChapterAccess, Ctx, SourceAdapter, build_adapter, builtin_presets};
use tankovault_domain::{ContentType, SeriesStatus};
use tankovault_fetch::{FetchError, FetchRequest, FetchResponse, Fetcher};

const QUERY: &str = include_str!("../fixtures/iken/query.json");
const POST: &str = include_str!("../fixtures/iken/post.json");
const CHAPTERS: &str = include_str!("../fixtures/iken/chapters.json");

/// Serves one fixture per endpoint. Unset fields serve an empty body, so each test supplies
/// only the documents the call under test actually fetches.
#[derive(Default)]
struct ApiFetcher {
    query: &'static str,
    post: &'static str,
    chapters: &'static str,
}

#[async_trait]
impl Fetcher for ApiFetcher {
    async fn get(&self, req: FetchRequest) -> Result<FetchResponse, FetchError> {
        let body = if req.url.contains("/api/query") {
            self.query
        } else if req.url.contains("/api/chapters") {
            self.chapters
        } else {
            self.post
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

fn preset_adapter(slug: &str, fetcher: ApiFetcher) -> (Box<dyn SourceAdapter>, Ctx) {
    let preset = builtin_presets()
        .into_iter()
        .find(|p| p.slug == slug)
        .unwrap_or_else(|| panic!("no shipped preset named {slug}"));
    let adapter = build_adapter(preset.adapter, preset.slug, &preset.config)
        .unwrap_or_else(|e| panic!("{slug} preset failed to build: {e}"));
    let ctx = Ctx {
        base_url: preset.base_url.to_owned(),
        provider_slug: preset.slug.to_owned(),
        fetcher: Arc::new(fetcher),
    };
    (adapter, ctx)
}

/// The catalogue walk ends on the platform's own `totalCount`, not on an empty page: novels
/// share these endpoints with comics and are filtered out here, so a page can legitimately
/// yield nothing while hundreds of series remain behind it.
#[tokio::test]
async fn the_catalogue_pages_on_the_platforms_own_total() {
    let (adapter, ctx) = preset_adapter(
        "vortexscans",
        ApiFetcher {
            query: QUERY,
            ..ApiFetcher::default()
        },
    );

    let first = adapter.list_catalog(&ctx, 1).await.expect("page 1 parses");
    assert_eq!(first.items.len(), 2);
    assert!(first.has_next, "687 series is more than one page of 40");
    assert!(
        first.items.iter().all(|i| i.path.starts_with("/series/")),
        "stored paths are the reader's, not the API's: {:?}",
        first.items.iter().map(|i| &i.path).collect::<Vec<_>>()
    );

    // Page 18 of 40 covers 720 > 687, so the walk stops there rather than on a short page.
    let past_end = adapter.list_catalog(&ctx, 18).await.expect("page parses");
    assert!(!past_end.has_next);
}

#[tokio::test]
async fn the_feed_lists_series_paths() {
    let (adapter, ctx) = preset_adapter(
        "vortexscans",
        ApiFetcher {
            query: QUERY,
            ..ApiFetcher::default()
        },
    );
    let updates = adapter.list_latest(&ctx).await.expect("feed parses");

    assert_eq!(updates.len(), 2);
    assert!(updates.iter().all(|u| u.path.starts_with("/series/")));
    assert!(updates.iter().all(|u| !u.title.is_empty()));
}

#[tokio::test]
async fn series_metadata_comes_from_the_detail_endpoint() {
    let (adapter, ctx) = preset_adapter(
        "vortexscans",
        ApiFetcher {
            post: POST,
            ..ApiFetcher::default()
        },
    );
    let meta = adapter
        .fetch_series(&ctx, "/series/why-i-quit-being-the-demon-king-q77htq6b")
        .await
        .expect("series parses");

    // Stored exactly as typed, newlines included — and the title becomes the matching key, so
    // an untidied one keys differently from the same title read anywhere else.
    assert_eq!(meta.title, "Why I Quit Being the Demon King");
    assert_eq!(meta.status, SeriesStatus::Ongoing);
    assert_eq!(meta.content_type, ContentType::Manhwa);
    assert!(!meta.tags.is_empty(), "genres arrive as objects with names");
    // The synopsis is stored as HTML; the tags must not reach the reader.
    let description = meta.description.expect("a synopsis is published");
    assert!(!description.contains('<'), "markup survived: {description}");
    assert!(description.starts_with("In a time of chaos"));
    assert!(
        meta.alt_titles.iter().all(|t| !t.trim().is_empty()),
        "the field is newline-separated and ends with one: {:?}",
        meta.alt_titles
    );
}

/// The chapter endpoint is keyed by the numeric series id, which appears only in the detail
/// document — so the adapter has to fetch that first. Keying it by the slug returns nothing,
/// and every series would ingest zero chapters without anything failing.
#[tokio::test]
async fn chapters_carry_the_paywall_the_platform_publishes() {
    let (adapter, ctx) = preset_adapter(
        "vortexscans",
        ApiFetcher {
            post: POST,
            chapters: CHAPTERS,
            ..ApiFetcher::default()
        },
    );
    let chapters = adapter
        .fetch_chapters(&ctx, "/series/why-i-quit-being-the-demon-king-q77htq6b")
        .await
        .expect("chapters parse");

    assert_eq!(chapters.len(), 3);
    assert!(
        chapters.iter().all(|c| c
            .path
            .starts_with("/series/why-i-quit-being-the-demon-king-q77htq6b/")),
        "chapter paths hang off the series path: {:?}",
        chapters.iter().map(|c| &c.path).collect::<Vec<_>>()
    );
    assert!(
        chapters.iter().all(|c| c.published_at.is_some()),
        "every row states `createdAt`"
    );
    // These rows are permanently paid with `unlockAt: null`. A missing date is not a date in
    // the past: reading it as "unlocks now" would put chapters the reader cannot open into
    // their unread count, which is the exact failure the early-access model exists to prevent.
    assert!(
        chapters
            .iter()
            .any(|c| c.access == ChapterAccess::EarlyAccess { unlocks_at: None }),
        "the locked rows must stay locked: {:?}",
        chapters.iter().map(|c| c.access).collect::<Vec<_>>()
    );
    assert!(
        chapters.iter().any(|c| c.access == ChapterAccess::Free),
        "the unlocked row must read as free"
    );
}
