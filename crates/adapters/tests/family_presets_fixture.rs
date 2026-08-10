//! Fixture tests for the provider families added alongside the 2026-08 source expansion —
//! MangaThemesia, Manganato and the two config-only sites that are not on a shared theme.
//!
//! Every fixture is markup trimmed from a live response (solver-fetched where the origin is
//! behind bot management), and every assertion runs the *shipped* preset config through
//! `build_adapter`. A site that changes its layout therefore fails a test here rather than
//! quietly writing wrong rows into `series`/`chapters`.

use async_trait::async_trait;
use std::sync::Arc;
use tankovault_adapters::{Ctx, SourceAdapter, build_adapter, builtin_presets};
use tankovault_fetch::{FetchError, FetchRequest, FetchResponse, Fetcher};

/// Serves one fixture per URL shape. Unset fields serve an empty body, so each test supplies
/// only the documents the call under test actually fetches.
#[derive(Default)]
struct SiteFetcher {
    catalog: &'static str,
    series: &'static str,
    /// Body for the Manganato family's JSON chapter endpoint (`/api/manga/{slug}/chapters`).
    chapters_api: &'static str,
}

#[async_trait]
impl Fetcher for SiteFetcher {
    async fn get(&self, req: FetchRequest) -> Result<FetchResponse, FetchError> {
        let body = if req.url.contains("/api/manga/") {
            self.chapters_api
        } else if req.url.contains("page=")
            || req.url.contains("/projects")
            || req.url.contains("/page/")
        {
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

// -----------------------------------------------------------------------------------------
// MangaThemesia — rizzfables
// -----------------------------------------------------------------------------------------

const THEMESIA_CATALOG: &str = include_str!("../fixtures/mangathemesia/catalog.html");
const THEMESIA_SERIES: &str = include_str!("../fixtures/mangathemesia/series.html");

#[tokio::test]
async fn mangathemesia_catalogue_reads_the_full_title_from_the_anchor() {
    let (adapter, ctx) = preset_adapter(
        "rizzfables",
        SiteFetcher {
            catalog: THEMESIA_CATALOG,
            ..SiteFetcher::default()
        },
    );
    let page = adapter.list_catalog(&ctx, 1).await.expect("catalogue parses");

    assert_eq!(page.items.len(), 3, "one item per div.bsx");
    // The visible caption is CSS-clipped; the anchor's `title` is the only place the untruncated
    // title exists, which is why the family default selects `a@title` and not the link text.
    assert!(
        page.items.iter().all(|i| !i.title.is_empty()),
        "every item needs a title: {:?}",
        page.items.iter().map(|i| &i.title).collect::<Vec<_>>()
    );
    assert!(
        page.items.iter().all(|i| i.path.starts_with("/series/")),
        "series links are site-relative: {:?}",
        page.items.iter().map(|i| &i.path).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn mangathemesia_series_reads_the_labelled_info_rows() {
    let (adapter, ctx) = preset_adapter(
        "rizzfables",
        SiteFetcher {
            series: THEMESIA_SERIES,
            ..SiteFetcher::default()
        },
    );
    let meta = adapter
        .fetch_series(&ctx, "/series/r2311170-mercenary-enrollment")
        .await
        .expect("series parses");

    assert_eq!(meta.title, "Mercenary Enrollment");
    assert!(!meta.tags.is_empty(), "genres come from span.mgen a");
    // `div.imptdt` renders Status, Type, Released, Author and Artist as structurally identical
    // rows whose only distinguishing feature is their leading text. Selecting the row without
    // matching that text stores the label itself as the value — the bug `madara`'s `alt` row
    // documents, reproduced here because this theme has the same shape.
    assert!(
        meta.authors.iter().all(|a| !a.eq_ignore_ascii_case("author")),
        "the row label must not survive as an author: {:?}",
        meta.authors
    );
    assert_eq!(meta.release_year, Some(2024));
}

#[tokio::test]
async fn mangathemesia_chapters_carry_number_title_and_date() {
    let (adapter, ctx) = preset_adapter(
        "rizzfables",
        SiteFetcher {
            series: THEMESIA_SERIES,
            ..SiteFetcher::default()
        },
    );
    let chapters = adapter
        .fetch_chapters(&ctx, "/series/r2311170-mercenary-enrollment")
        .await
        .expect("chapters parse");

    assert_eq!(chapters.len(), 3);
    assert!(
        chapters.iter().all(|c| c.number > 0.0 && c.number.is_finite()),
        "every row needs a finite number: {:?}",
        chapters.iter().map(|c| c.number).collect::<Vec<_>>()
    );
    assert!(
        chapters.iter().all(|c| c.path.starts_with("/chapter/")),
        "chapter links are site-relative: {:?}",
        chapters.iter().map(|c| &c.path).collect::<Vec<_>>()
    );
    // This theme sells no early access, so the shipped config carries no `locked` selector and
    // every row must read as free. A default of "locked" here would empty the unread count.
    assert!(
        chapters
            .iter()
            .all(|c| c.access == tankovault_adapters::ChapterAccess::Free),
        "no lock selector configured means every chapter is free"
    );
}

// -----------------------------------------------------------------------------------------
// Manganato — natomanga
// -----------------------------------------------------------------------------------------

const NATO_CATALOG: &str = include_str!("../fixtures/manganato/catalog.html");
const NATO_SERIES: &str = include_str!("../fixtures/manganato/series.html");
const NATO_CHAPTERS: &str = include_str!("../fixtures/manganato/chapters.json");

#[tokio::test]
async fn manganato_catalogue_lists_series_links() {
    let (adapter, ctx) = preset_adapter(
        "natomanga",
        SiteFetcher {
            catalog: NATO_CATALOG,
            ..SiteFetcher::default()
        },
    );
    let page = adapter.list_catalog(&ctx, 2).await.expect("catalogue parses");

    assert_eq!(page.items.len(), 3);
    assert!(
        page.items.iter().all(|i| i.path.starts_with("/manga/")),
        "{:?}",
        page.items.iter().map(|i| &i.path).collect::<Vec<_>>()
    );
    assert!(page.items.iter().all(|i| !i.title.is_empty()));
}

#[tokio::test]
async fn manganato_series_splits_the_label_off_its_value() {
    let (adapter, ctx) = preset_adapter(
        "natomanga",
        SiteFetcher {
            series: NATO_SERIES,
            ..SiteFetcher::default()
        },
    );
    let meta = adapter
        .fetch_series(&ctx, "/manga/your-regrets-mean-nothing-to-me")
        .await
        .expect("series parses");

    assert_eq!(meta.title, "Your Regrets Mean Nothing to Me");
    // This family renders `<li>Author(s) : Park Hae-nae</li>` — label and value in one text
    // node. Reading the row verbatim stores `Author(s) : Park Hae-nae` as the author name, and
    // the same shape feeds `series_titles` for alternatives, where a wrong key reaches the
    // trigram matcher and catalogue search.
    assert!(
        meta.authors.iter().any(|a| a.contains("Park Hae-nae")),
        "author must be the value, not the row: {:?}",
        meta.authors
    );
    assert!(
        !meta.authors.iter().any(|a| a.contains("Author")),
        "the label must not survive into the value: {:?}",
        meta.authors
    );
    assert!(
        meta.alt_titles
            .iter()
            .any(|t| t.contains("I Won't Accept Your Regrets")),
        "alternatives come from the labelled `Alternative :` row: {:?}",
        meta.alt_titles
    );
    assert!(!meta.tags.is_empty(), "genres come from li.genres a");
}

/// Regression guard for the family's defining trap: the series page carries only the newest
/// ~25 chapters and the rest arrive from a JSON endpoint. An adapter that reads the markup
/// instead silently truncates every long series — a chapter count that looks plausible and is
/// wrong, which no error path would ever report.
#[tokio::test]
async fn manganato_chapters_come_from_the_json_endpoint_not_the_page() {
    let (adapter, ctx) = preset_adapter(
        "natomanga",
        SiteFetcher {
            // Deliberately empty: if the adapter ever falls back to scraping the series page,
            // this test fails with zero chapters instead of passing on a partial list.
            series: "",
            chapters_api: NATO_CHAPTERS,
            ..SiteFetcher::default()
        },
    );
    let chapters = adapter
        .fetch_chapters(&ctx, "/manga/your-regrets-mean-nothing-to-me")
        .await
        .expect("chapters parse");

    assert!(
        chapters.len() > 25,
        "the endpoint returns the whole list, not the page's window: {}",
        chapters.len()
    );
    assert!(
        chapters.iter().all(|c| c.number.is_finite() && c.number > 0.0),
        "every chapter needs a finite number"
    );
    assert!(
        chapters.iter().any(|c| c.published_at.is_some()),
        "the endpoint publishes `updated_at`, so dates must survive"
    );
    assert!(
        chapters
            .iter()
            .all(|c| c.path.starts_with("/manga/your-regrets-mean-nothing-to-me/")),
        "chapter paths hang off the series path"
    );
    // Sub-chapter numbering is the reason `number` is `numeric(10,4)` and not an integer.
    assert!(
        chapters.iter().any(|c| c.number.fract() > 0.0),
        "this series publishes sub-chapters; they must not round to whole numbers"
    );
}

// -----------------------------------------------------------------------------------------
// TCB Scans — a single-page catalogue
// -----------------------------------------------------------------------------------------

const TCB_CATALOG: &str = include_str!("../fixtures/tcbscans/catalog.html");
const TCB_SERIES: &str = include_str!("../fixtures/tcbscans/series.html");

/// The catalogue is one page, and the site answers *any* page number with it. Without the
/// preset's `pages: 1`, `has_next` falls back to "this page yielded items" and the walk
/// re-fetches the same page until the planner's cap, re-ingesting all 19 series each time.
#[tokio::test]
async fn tcb_catalogue_is_one_page_and_says_so() {
    let (adapter, ctx) = preset_adapter(
        "tcbscans",
        SiteFetcher {
            catalog: TCB_CATALOG,
            ..SiteFetcher::default()
        },
    );
    let page = adapter.list_catalog(&ctx, 1).await.expect("catalogue parses");

    assert_eq!(page.items.len(), 4);
    assert!(
        !page.has_next,
        "a declared one-page catalogue must never report another page"
    );
    assert!(page.items.iter().all(|i| i.path.starts_with("/mangas/")));
    // The list item *is* the anchor here, which is what the `self` link spec exists for: a
    // descendant selector finds nothing, and matching the parent instead would group all 19.
    assert!(
        page.items.iter().all(|i| !i.title.is_empty()),
        "titles come from the cover's alt text: {:?}",
        page.items.iter().map(|i| &i.title).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn tcb_series_and_chapters_parse() {
    let (adapter, ctx) = preset_adapter(
        "tcbscans",
        SiteFetcher {
            series: TCB_SERIES,
            ..SiteFetcher::default()
        },
    );
    let meta = adapter
        .fetch_series(&ctx, "/mangas/5/one-piece")
        .await
        .expect("series parses");
    assert_eq!(meta.title, "One Piece");
    assert!(
        meta.description.is_some_and(|d| d.contains("Monkey D. Luffy")),
        "the synopsis is the only prose block on the page"
    );

    let chapters = adapter
        .fetch_chapters(&ctx, "/mangas/5/one-piece")
        .await
        .expect("chapters parse");
    assert_eq!(chapters.len(), 4);
    assert!(chapters.iter().all(|c| c.path.starts_with("/chapters/")));
}

// -----------------------------------------------------------------------------------------
// Toonily — Madara with a different listing path
// -----------------------------------------------------------------------------------------

const TOONILY_CATALOG: &str = include_str!("../fixtures/toonily/catalog.html");
const TOONILY_SERIES: &str = include_str!("../fixtures/toonily/series.html");

#[tokio::test]
async fn toonily_catalogue_walks_the_search_listing() {
    let (adapter, ctx) = preset_adapter(
        "toonily",
        SiteFetcher {
            catalog: TOONILY_CATALOG,
            ..SiteFetcher::default()
        },
    );
    let page = adapter.list_catalog(&ctx, 2).await.expect("catalogue parses");

    assert_eq!(page.items.len(), 3);
    // `/manga/` is not a listing on this site — `/search/` is. A preset pointed at the Madara
    // default path yields an empty catalogue and a scan that succeeds having found nothing.
    assert!(
        page.items.iter().all(|i| i.path.starts_with("/serie/")),
        "{:?}",
        page.items.iter().map(|i| &i.path).collect::<Vec<_>>()
    );
    assert!(
        page.has_next,
        "this theme does render `a.nextpostslink`, so the default marker is kept"
    );
}

#[tokio::test]
async fn toonily_series_reads_the_lazy_loaded_cover() {
    let (adapter, ctx) = preset_adapter(
        "toonily",
        SiteFetcher {
            series: TOONILY_SERIES,
            ..SiteFetcher::default()
        },
    );
    let meta = adapter
        .fetch_series(&ctx, "/serie/solo-leveling-0a10cf7b/")
        .await
        .expect("series parses");

    // The `<h1>` also contains the theme's `END` badge. `@text` reads the heading's own text
    // nodes only — without it the canonical title, and therefore the matching key this series
    // is found by, becomes `Solo Leveling END`.
    assert_eq!(meta.title, "Solo Leveling");
    // This origin is always solver-rendered, so the cover arrives as a resolved `src`. The
    // `data-src` override that manhuaplus needs would find nothing here — hence no override,
    // and hence this assertion, which fails if one is ever added by analogy.
    let cover = meta.cover_url.expect("a cover must be found");
    assert!(
        cover.starts_with("https://static.tnlycdn.com/"),
        "the cover is the CDN image the rendered page carries: {cover}"
    );

    let chapters = adapter
        .fetch_chapters(&ctx, "/serie/solo-leveling-0a10cf7b/")
        .await
        .expect("chapters parse");
    assert_eq!(chapters.len(), 3);
    // `May 31, 23` — a month-name date, which is what the Madara themes render and what
    // `parse_date_label` was extended to read.
    assert!(
        chapters.iter().any(|c| c.published_at.is_some()),
        "month-name release dates must parse"
    );
}
