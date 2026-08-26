//! Fixture tests for the provider families added alongside the 2026-08 source expansion —
//! `MangaThemesia`, Manganato and the two config-only sites that are not on a shared theme.
//!
//! Every fixture is markup trimmed from a live response (solver-fetched where the origin is
//! behind bot management), and every assertion runs the *shipped* preset config through
//! `build_adapter`. A site that changes its layout therefore fails a test here rather than
//! quietly writing wrong rows into `series`/`chapters`.

use async_trait::async_trait;
use std::sync::Arc;
use tankovault_adapters::{Ctx, SourceAdapter, build_adapter, builtin_presets};
use tankovault_domain::SeriesStatus;
use tankovault_fetch::{FetchError, FetchRequest, FetchResponse, Fetcher};

/// Serves one fixture per URL shape. Unset fields serve an empty body, so each test supplies
/// only the documents the call under test actually fetches.
#[derive(Default)]
struct SiteFetcher {
    catalog: &'static str,
    series: &'static str,
    /// Body for the Manganato family's JSON chapter endpoint (`/api/manga/{slug}/chapters`).
    chapters_api: &'static str,
    /// Body for a feed served from a URL of its own — for Keyoapp that is the site root.
    latest: &'static str,
}

/// Whether `url` addresses the site root itself (`https://host` or `https://host/`).
fn is_site_root(url: &str) -> bool {
    url.split_once("://")
        .is_some_and(|(_, rest)| !rest.trim_end_matches('/').contains('/'))
}

#[async_trait]
impl Fetcher for SiteFetcher {
    async fn get(&self, req: FetchRequest) -> Result<FetchResponse, FetchError> {
        // A sitemap shard past the last one 404s; that is the site saying the catalogue ended.
        if req.url.contains("sitemap") && !req.url.contains("sitemap1") {
            return Ok(FetchResponse {
                status: 404,
                url: req.url.clone(),
                headers: Vec::new(),
                body: "404 page not found".to_owned(),
                from_cache: false,
            });
        }
        let body = if req.url.contains("/api/manga/") {
            self.chapters_api
        } else if req.url.contains("/latest/") || is_site_root(&req.url) {
            // Keyoapp's feed is the home page, so "the request is for the origin and nothing
            // else" is what identifies it. Matching on a bare "/" would match every path.
            self.latest
        } else if req.url.ends_with("/series/") {
            // Keyoapp's whole catalogue is this one document; a *series* page is one segment
            // deeper, which is the only thing that tells the two apart.
            self.catalog
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
/// A `rokaricomics` series page: same theme, plus the coin plugin's price badge on a paid row.
const ROKARI_SERIES: &str = include_str!("../fixtures/mangathemesia/rokari-series.html");

#[tokio::test]
async fn mangathemesia_catalogue_reads_the_full_title_from_the_anchor() {
    let (adapter, ctx) = preset_adapter(
        "rizzfables",
        SiteFetcher {
            catalog: THEMESIA_CATALOG,
            ..SiteFetcher::default()
        },
    );
    let page = adapter
        .list_catalog(&ctx, 1)
        .await
        .expect("catalogue parses");

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
        meta.authors
            .iter()
            .all(|a| !a.eq_ignore_ascii_case("author")),
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
        chapters
            .iter()
            .all(|c| c.number > 0.0 && c.number.is_finite()),
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

/// Regression: `rokaricomics` sells early access through a coin plugin the stock theme knows
/// nothing about, and its preset carried no `locked` selector — so every paid chapter ingested
/// as free, and the reader's next unread page was a paywall. A paid row keeps a working chapter
/// link and differs from a free one by one `span.text-gold` carrying the coin glyph and price,
/// which is why that badge, and not the link, has to be what decides access.
#[tokio::test]
async fn rokari_reads_the_coin_badge_as_a_lock() {
    let (adapter, ctx) = preset_adapter(
        "rokaricomics",
        SiteFetcher {
            series: ROKARI_SERIES,
            ..SiteFetcher::default()
        },
    );
    let chapters = adapter
        .fetch_chapters(&ctx, "/manga/the-heavenly-demon-cults-strongest-maid/")
        .await
        .expect("chapters parse");

    // The badge decorates the row; it must not cost the row its link, or the chapter would
    // vanish from the list instead of appearing as locked.
    assert_eq!(chapters.len(), 3);
    assert!(
        chapters.iter().all(|c| !c.path.is_empty()),
        "every row keeps its chapter link: {:?}",
        chapters.iter().map(|c| &c.path).collect::<Vec<_>>()
    );

    let badged = chapters
        .iter()
        .find(|c| (c.number - 37.0).abs() < f64::EPSILON)
        .expect("chapter 37 is in the fixture");
    // The badge prices the chapter and never dates it, so the unlock time stays unknown — which
    // keeps the chapter locked rather than freeing it on a date nobody published.
    assert_eq!(
        badged.access,
        tankovault_adapters::ChapterAccess::EarlyAccess { unlocks_at: None },
        "the priced row is early access: {:?}",
        chapters
            .iter()
            .map(|c| (c.number, c.access))
            .collect::<Vec<_>>()
    );
    // And the unbadged rows stay free: a marker that matched every row would empty the unread
    // count for the whole provider.
    assert!(
        chapters
            .iter()
            .filter(|c| (c.number - 37.0).abs() >= f64::EPSILON)
            .all(|c| c.access == tankovault_adapters::ChapterAccess::Free),
        "rows with no badge are free: {:?}",
        chapters
            .iter()
            .map(|c| (c.number, c.access))
            .collect::<Vec<_>>()
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
    let page = adapter
        .list_catalog(&ctx, 2)
        .await
        .expect("catalogue parses");

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
        chapters
            .iter()
            .all(|c| c.number.is_finite() && c.number > 0.0),
        "every chapter needs a finite number"
    );
    assert!(
        chapters.iter().any(|c| c.published_at.is_some()),
        "the endpoint publishes `updated_at`, so dates must survive"
    );
    assert!(
        chapters.iter().all(|c| c
            .path
            .starts_with("/manga/your-regrets-mean-nothing-to-me/")),
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
    let page = adapter
        .list_catalog(&ctx, 1)
        .await
        .expect("catalogue parses");

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
        meta.description
            .is_some_and(|d| d.contains("Monkey D. Luffy")),
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
    let page = adapter
        .list_catalog(&ctx, 2)
        .await
        .expect("catalogue parses");

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

/// Regression: a catalogue that does not paginate must say so, or the walk never terminates.
///
/// Rizz Fables lists all 88 of its series on one page and answers every `?page=N` with that
/// same document. The `MangaThemesia` family default clears `catalog.next`, so `has_next` falls
/// back to "this page yielded items" — which is true forever on a site like this. A full scan
/// re-fetched and re-ingested page 1 until the planner's page cap: twenty thousand requests for
/// eighty-eight series, with no error anywhere, because every page genuinely succeeded.
///
/// Found by a live full scan, not by a fast scan — the fast path never calls `list_catalog`.
#[tokio::test]
async fn a_single_page_catalogue_reports_no_next_page() {
    let (adapter, ctx) = preset_adapter(
        "rizzfables",
        SiteFetcher {
            catalog: THEMESIA_CATALOG,
            ..SiteFetcher::default()
        },
    );

    let first = adapter.list_catalog(&ctx, 1).await.expect("page 1 parses");
    assert!(!first.items.is_empty(), "page 1 still yields the catalogue");
    assert!(
        !first.has_next,
        "a declared one-page catalogue must never report another page, however many items it has"
    );
}

/// Regression: in sitemap mode a 404 on shard `n+1` is how the catalogue ends, not a failure.
///
/// `MangaPill` publishes one shard. Reported as an error, the walk ended *and* the run was marked
/// degraded — a scan failure logged on every full scan, forever, for a provider behaving
/// perfectly. Restricted to 404 on purpose: a 403 or a 5xx means part of the catalogue was not
/// seen, and must keep surfacing.
#[tokio::test]
async fn a_missing_sitemap_shard_ends_the_walk_without_failing_it() {
    let (adapter, ctx) = preset_adapter("mangapill", SiteFetcher::default());

    let past_end = adapter
        .list_catalog(&ctx, 2)
        .await
        .expect("a missing shard must not be an error");
    assert!(past_end.items.is_empty());
    assert!(!past_end.has_next, "there is nothing after a missing shard");
}

// -----------------------------------------------------------------------------------------
// Keyoapp — asmotoon
// -----------------------------------------------------------------------------------------

const KEYO_CATALOG: &str = include_str!("../fixtures/keyoapp/catalog.html");
const KEYO_LATEST: &str = include_str!("../fixtures/keyoapp/latest.html");
const KEYO_SERIES: &str = include_str!("../fixtures/keyoapp/series.html");

/// The platform renders its whole catalogue into one document and filters it in the browser, so
/// `pages: 1` is what ends the walk. Without it the yielded-items fallback never goes false —
/// the same document answers every page number — and the walk re-ingests it until the planner's
/// cap, with every request "succeeding".
#[tokio::test]
async fn keyoapp_catalogue_is_one_page_and_says_so() {
    let (adapter, ctx) = preset_adapter(
        "asmotoon",
        SiteFetcher {
            catalog: KEYO_CATALOG,
            ..SiteFetcher::default()
        },
    );
    let page = adapter
        .list_catalog(&ctx, 1)
        .await
        .expect("catalogue parses");

    assert_eq!(
        page.items.len(),
        3,
        "one item per #searched_series_page button"
    );
    assert!(!page.has_next, "the catalogue is a single document");
    assert!(
        page.items.iter().all(|i| i.path.starts_with("/series/")),
        "{:?}",
        page.items.iter().map(|i| &i.path).collect::<Vec<_>>()
    );
    // The button's own `title` concatenates the title with every alternative; the anchor's
    // carries the canonical one alone, and that is what the matching key is built from.
    assert!(
        page.items
            .iter()
            .all(|i| !i.title.is_empty() && !i.title.contains("Ponkotsu")),
        "titles come from the anchor, not the button: {:?}",
        page.items.iter().map(|i| &i.title).collect::<Vec<_>>()
    );
}

/// Regression: the feed read `/latest/`, which every install answers with its **entire**
/// catalogue re-sorted by update time — 729 cards on the largest — so a fast scan fanned out a
/// child task per series in the catalogue, every cycle. Production also saw eight of the nine
/// installs answer that route with an origin `404` while `/series/` on the same host succeeded.
/// The home page's `#latest` strip is a dozen cards and cannot 404.
///
/// The scoping is the load-bearing part: `div.latest-poster` is the card class for Trending,
/// Pinned and Recently Added as well, and none of those is an update.
#[tokio::test]
async fn keyoapp_latest_feed_is_the_home_pages_latest_strip_only() {
    let (adapter, ctx) = preset_adapter(
        "asmotoon",
        SiteFetcher {
            latest: KEYO_LATEST,
            ..SiteFetcher::default()
        },
    );
    let updates = adapter.list_latest(&ctx).await.expect("feed parses");

    assert_eq!(updates.len(), 2, "only the two cards inside `#latest`");
    assert!(
        !updates
            .iter()
            .any(|u| u.path.contains("vengeful-villain-slayer")),
        "the Trending card sits outside `#latest` and is not an update: {:?}",
        updates.iter().map(|u| &u.path).collect::<Vec<_>>()
    );
    // Each card links to both the series and its newest chapters. A feed that registered the
    // chapter link as a series path would store series that can never have chapters, since a
    // chapter page carries no chapter list.
    assert!(
        updates.iter().all(|u| u.path.starts_with("/series/")),
        "{:?}",
        updates.iter().map(|u| &u.path).collect::<Vec<_>>()
    );
    // Read off the anchor's `title`, not an `h3`: several installs render the name in a plain
    // `span` and the card then had no title at all.
    assert!(updates.iter().all(|u| !u.title.is_empty()));
    assert!(
        updates.iter().all(|u| u.latest_chapter > 0.0),
        "the card's chapter link carries `Chapter N` in its title attribute: {:?}",
        updates.iter().map(|u| u.latest_chapter).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn keyoapp_series_reads_the_labelled_info_rows() {
    let (adapter, ctx) = preset_adapter(
        "asmotoon",
        SiteFetcher {
            series: KEYO_SERIES,
            ..SiteFetcher::default()
        },
    );
    let meta = adapter
        .fetch_series(
            &ctx,
            "/series/c-level-magic-student-who-thinks-he-is-sss-class/",
        )
        .await
        .expect("series parses");

    assert_eq!(
        meta.title,
        "C-level Magic Student Who Thinks He Is SSS Class"
    );
    assert_eq!(meta.status, SeriesStatus::Ongoing);
    // Author, Artist, Type and Status render as structurally identical two-cell grids whose
    // only distinguishing feature is the label text. Selecting the row without matching that
    // text stores the labels themselves — into `series_titles` for `alt`, whose `normalized`
    // column the trigram matcher and catalogue search both score against.
    assert_eq!(meta.authors, vec!["NKMR".to_owned()]);
    assert!(
        meta.alt_titles
            .iter()
            .all(|t| !t.eq_ignore_ascii_case("alternative titles")),
        "the label must not survive as an alternative title: {:?}",
        meta.alt_titles
    );
    assert!(meta.alt_titles.iter().any(|t| t.contains("Jibun Wo SSS")));
    // The cover is a CSS `background-image`; the Open Graph tag is the only readable copy.
    assert!(
        meta.cover_url
            .as_deref()
            .is_some_and(|u| u.contains("http")),
        "cover comes from og:image: {:?}",
        meta.cover_url
    );
}

/// The row's own `d` attribute is the date, and `self@d` is the only way to reach it: the
/// rendered copy sits behind two other `.text-xs` elements (a "New" badge and the coin price)
/// that a first-match selector picks up instead.
#[tokio::test]
async fn keyoapp_chapters_carry_the_row_attribute_date_and_the_paywall() {
    let (adapter, ctx) = preset_adapter(
        "asmotoon",
        SiteFetcher {
            series: KEYO_SERIES,
            ..SiteFetcher::default()
        },
    );
    let chapters = adapter
        .fetch_chapters(
            &ctx,
            "/series/c-level-magic-student-who-thinks-he-is-sss-class/",
        )
        .await
        .expect("chapters parse");

    assert_eq!(chapters.len(), 3);
    assert!(
        chapters.iter().all(|c| c.path.starts_with("/chapter/")),
        "{:?}",
        chapters.iter().map(|c| &c.path).collect::<Vec<_>>()
    );
    assert!(
        chapters.iter().all(|c| c.published_at.is_some()),
        "every row states a date in `d`: {:?}",
        chapters.iter().map(|c| c.published_at).collect::<Vec<_>>()
    );
    // The platform states a price and never a date, so a locked chapter has no unlock time —
    // and must stay locked rather than being read as "unlocks now".
    let locked: Vec<_> = chapters
        .iter()
        .filter(|c| c.access != tankovault_adapters::ChapterAccess::Free)
        .collect();
    assert_eq!(
        locked.len(),
        2,
        "two of the three rows carry the coin badge: {:?}",
        chapters
            .iter()
            .map(|c| (c.number, c.access))
            .collect::<Vec<_>>()
    );
    assert!(
        locked
            .iter()
            .all(|c| c.access
                == tankovault_adapters::ChapterAccess::EarlyAccess { unlocks_at: None }),
        "a price is not a date, so a paid row must stay locked indefinitely"
    );
    // And the unbadged row must still read as free — a marker that matched every row would
    // empty the unread count for the whole provider.
    assert!(
        chapters
            .iter()
            .any(|c| c.access == tankovault_adapters::ChapterAccess::Free),
        "the row with no coin badge is free"
    );
}
