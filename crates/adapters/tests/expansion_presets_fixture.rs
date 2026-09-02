//! Fixture tests for the 2026-08-26 source expansion.
//!
//! Every fixture is markup trimmed from a live response and every assertion runs the **shipped**
//! preset through `build_adapter`, so a layout change fails a test here rather than quietly
//! writing wrong rows into `series`/`chapters`.
//!
//! What is pinned is deliberately narrow: the selector on each site that a plausible-looking
//! alternative gets *silently* wrong. Nothing here re-tests the family defaults — those have
//! their own suites — and nothing here asserts a value the site is free to change, like a title
//! or a chapter count that grows every week.

use async_trait::async_trait;
use std::sync::{Arc, Mutex};
use tankovault_adapters::{ChapterAccess, Ctx, SourceAdapter, build_adapter, builtin_presets};
use tankovault_fetch::{FetchError, FetchRequest, FetchResponse, Fetcher};

/// Serves one fixture per URL shape and records every URL asked for.
///
/// The recording is what lets a test assert *where* an adapter looked, which for `mgeko` is the
/// whole point: reading the series page instead of its all-chapters route parses cleanly and
/// truncates the list.
#[derive(Default)]
struct SiteFetcher {
    catalog: &'static str,
    series: &'static str,
    latest: &'static str,
    /// Body for a chapter list served from a URL of its own.
    chapters: &'static str,
    seen: Mutex<Vec<String>>,
}

impl SiteFetcher {
    fn new(catalog: &'static str, series: &'static str) -> Self {
        Self {
            catalog,
            series,
            ..Self::default()
        }
    }
}

/// Whether `url` addresses the site root itself (`https://host` or `https://host/`).
fn is_site_root(url: &str) -> bool {
    url.split_once("://")
        .is_some_and(|(_, rest)| !rest.trim_end_matches('/').contains('/'))
}

#[async_trait]
impl Fetcher for SiteFetcher {
    async fn get(&self, req: FetchRequest) -> Result<FetchResponse, FetchError> {
        self.seen
            .lock()
            .expect("fixture fetcher lock")
            .push(req.url.clone());
        let body = if req.url.contains("all-chapters") {
            self.chapters
        } else if is_site_root(&req.url) {
            self.latest
        } else if req.url.contains("page=")
            || req.url.contains("/page/")
            || req.url.contains("az-list")
            || req.url.ends_with("/manga/")
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
fn preset_adapter(
    slug: &str,
    fetcher: SiteFetcher,
) -> (Box<dyn SourceAdapter>, Ctx, Arc<SiteFetcher>) {
    let preset = builtin_presets()
        .into_iter()
        .find(|p| p.slug == slug)
        .unwrap_or_else(|| panic!("no shipped preset named {slug}"));
    let adapter = build_adapter(preset.adapter, preset.slug, &preset.config)
        .unwrap_or_else(|e| panic!("{slug} preset failed to build: {e}"));
    let fetcher = Arc::new(fetcher);
    let ctx = Ctx {
        base_url: preset.base_url.to_owned(),
        provider_slug: preset.slug.to_owned(),
        fetcher: Arc::clone(&fetcher) as Arc<dyn Fetcher>,
    };
    (adapter, ctx, fetcher)
}

/// How many of `chapters` the provider says are behind a paywall.
fn locked(chapters: &[tankovault_adapters::ChapterMeta]) -> usize {
    chapters
        .iter()
        .filter(|c| matches!(c.access, ChapterAccess::EarlyAccess { .. }))
        .count()
}

// -----------------------------------------------------------------------------------------
// Early access — the selectors that decide whether a reader is sent to a paywall
// -----------------------------------------------------------------------------------------

const MADARASCANS_CATALOG: &str = include_str!("../fixtures/expansion/madarascans-catalog.html");
const MADARASCANS_SERIES: &str = include_str!("../fixtures/expansion/madarascans-series.html");
const ATHREASCANS_SERIES: &str = include_str!("../fixtures/expansion/athreascans-series.html");

/// Regression: `madarascans` sells early access and marks a paid row with a lock glyph inside the
/// number cell — nowhere else. On the family defaults every paid chapter parsed as free, which is
/// the one parse error that reaches a reader: an unread count, a release-feed entry and a
/// "next up" marker all pointing at a paywall.
///
/// The fixture carries both shapes on purpose. A selector that matched the row container rather
/// than the glyph would mark all four locked and still pass a "locked > 0" assertion.
#[tokio::test]
async fn madarascans_marks_only_the_lock_glyphed_rows_as_early_access() {
    let (adapter, ctx, _) = preset_adapter(
        "madarascans",
        SiteFetcher::new(MADARASCANS_CATALOG, MADARASCANS_SERIES),
    );
    let chapters = adapter
        .fetch_chapters(&ctx, "/series/whatever/")
        .await
        .expect("chapter list parses");

    assert_eq!(chapters.len(), 4, "one chapter per [data-ch] row");
    assert_eq!(locked(&chapters), 3, "three rows carry the lock glyph");
    // The site states a price, never a date. An unlock time invented here would open the chapter
    // in every read path the moment the row was stored.
    assert!(
        chapters.iter().all(|c| !matches!(
            c.access,
            ChapterAccess::EarlyAccess {
                unlocks_at: Some(_)
            }
        )),
        "no row announces an unlock date, so none may carry one"
    );
}

/// The same rows must still yield a usable chapter link: the anchor is one level inside the row
/// here, so a `link` selector naming the row itself finds no href and the list parses to zero.
#[tokio::test]
async fn madarascans_reads_the_chapter_link_from_the_inner_anchor() {
    let (adapter, ctx, _) = preset_adapter(
        "madarascans",
        SiteFetcher::new(MADARASCANS_CATALOG, MADARASCANS_SERIES),
    );
    let chapters = adapter
        .fetch_chapters(&ctx, "/series/whatever/")
        .await
        .expect("chapter list parses");

    assert!(
        chapters.iter().all(|c| c.path.starts_with('/')),
        "every row stores a relative chapter path: {:?}",
        chapters.iter().map(|c| &c.path).collect::<Vec<_>>()
    );
}

/// `madarascans` replaced the theme's `div.bsx` grid, so the catalogue is its own selector set.
#[tokio::test]
async fn madarascans_catalogue_reads_the_rewritten_card() {
    let (adapter, ctx, _) = preset_adapter(
        "madarascans",
        SiteFetcher::new(MADARASCANS_CATALOG, MADARASCANS_SERIES),
    );
    let page = adapter
        .list_catalog(&ctx, 1)
        .await
        .expect("catalogue parses");

    assert_eq!(page.items.len(), 3, "one item per div.manga-card-v");
    assert!(
        page.items
            .iter()
            .all(|i| !i.title.is_empty() && i.path.contains("/series/")),
        "each card yields a titled series path: {:?}",
        page.items
            .iter()
            .map(|i| (&i.title, &i.path))
            .collect::<Vec<_>>()
    );
}

/// `athreascans` runs the `MangaThemesia` coin plugin, and this is why its preset carries no
/// `chapters.locked` selector: a paid row here is a modal trigger whose anchor has **no `href`**,
/// so the row yields no chapter URL and is dropped before access is read. Only the free rows are
/// ingested, which is the safe outcome — a paywalled chapter is never offered because it is never
/// stored.
///
/// Pinned because the failure mode if the install changes is silent and one-directional: the day
/// those rows gain an href, they start ingesting as **free** and go straight into unread counts.
/// This test fails then, and the fix is the `span.text-gold` rule `rokaricomics` already ships.
#[tokio::test]
async fn athreascans_drops_paid_rows_because_they_carry_no_chapter_link() {
    let (adapter, ctx, _) = preset_adapter("athreascans", SiteFetcher::new("", ATHREASCANS_SERIES));
    let chapters = adapter
        .fetch_chapters(&ctx, "/manga/whatever/")
        .await
        .expect("chapter list parses");

    // The fixture holds four rows: two coin-badged modal triggers and two ordinary links.
    assert_eq!(chapters.len(), 2, "only the linked rows can be stored");
    assert_eq!(
        locked(&chapters),
        0,
        "what survives is free; if a paid row ever reaches here it must not read as free"
    );
}

/// The `table.infotable` override is what makes this install publish any metadata at all: on the
/// theme's stock `div.imptdt` selectors it parsed a title, a cover and a description and nothing
/// else — no status, no genres, no credits, and no error, because an optional row that matches
/// nothing is how the theme is meant to read.
#[tokio::test]
async fn athreascans_reads_the_infotable_metadata_block() {
    let (adapter, ctx, _) = preset_adapter("athreascans", SiteFetcher::new("", ATHREASCANS_SERIES));
    let meta = adapter
        .fetch_series(&ctx, "/manga/whatever/")
        .await
        .expect("series parses");

    assert!(!meta.tags.is_empty(), "genres come from div.seriestugenre");
    assert!(
        !meta
            .alt_titles
            .iter()
            .any(|t| t.eq_ignore_ascii_case("alternative")),
        "the label cell must not be stored as a value: {:?}",
        meta.alt_titles
    );
}

// -----------------------------------------------------------------------------------------
// The silent-empty-parse shapes
// -----------------------------------------------------------------------------------------

const ZAZAMANGA_SERIES: &str = include_str!("../fixtures/expansion/zazamanga-series.html");
const ZINMANGA_LATEST: &str = include_str!("../fixtures/expansion/zinmanga-latest.html");

/// Regression: `zazamanga` renders the Madara chapter list as `div`, not the theme's `li`. On the
/// family default the site parsed a full catalogue, a full feed and **zero** chapters — and an
/// empty chapter list is a valid answer, so nothing failed.
#[tokio::test]
async fn zazamanga_reads_chapter_rows_rendered_as_divs() {
    let (adapter, ctx, _) = preset_adapter("zazamanga", SiteFetcher::new("", ZAZAMANGA_SERIES));
    let chapters = adapter
        .fetch_chapters(&ctx, "/manga/whatever/")
        .await
        .expect("chapter list parses");

    assert_eq!(chapters.len(), 3, "one chapter per div.wp-manga-chapter");
}

/// Regression: `zinmanga` runs the platform `kunmanga` does, and the first draft of its preset
/// inherited kunmanga's `latest` override — a bespoke home-page slider that this install does not
/// render. The feed parsed to zero items on every fast scan. It renders the theme's own cards, so
/// the correct preset overrides nothing here.
#[tokio::test]
async fn zinmanga_reads_its_feed_from_the_madara_default_cards() {
    let (adapter, ctx, _) = preset_adapter(
        "zinmanga",
        SiteFetcher {
            latest: ZINMANGA_LATEST,
            ..SiteFetcher::default()
        },
    );
    let items = adapter.list_latest(&ctx).await.expect("feed parses");

    assert_eq!(items.len(), 3, "one item per div.page-item-detail");
    assert!(
        items.iter().all(|i| !i.title.is_empty()),
        "every feed item needs a title: {:?}",
        items.iter().map(|i| &i.title).collect::<Vec<_>>()
    );
}

// -----------------------------------------------------------------------------------------
// Chapter lists that are not on the series page
// -----------------------------------------------------------------------------------------

const MGEKO_CHAPTERS: &str = include_str!("../fixtures/expansion/mgeko-chapters.html");
const KALISCAN_SERIES: &str = include_str!("../fixtures/expansion/kaliscan-series.html");

/// `mgeko` renders the newest rows on the series page behind a "Load All Chapters" control. The
/// series page therefore parses cleanly and truncates every series to its first screen; the
/// preset points `chapters.path` at what that control links to instead.
#[tokio::test]
async fn mgeko_fetches_the_all_chapters_route_and_not_the_series_page() {
    let (adapter, ctx, fetcher) = preset_adapter(
        "mgeko",
        SiteFetcher {
            series: "",
            chapters: MGEKO_CHAPTERS,
            ..SiteFetcher::default()
        },
    );
    let chapters = adapter
        .fetch_chapters(&ctx, "/manga/whatever/")
        .await
        .expect("chapter list parses");

    assert_eq!(chapters.len(), 3, "one chapter per ul.chapter-list li");
    let seen = fetcher.seen.lock().expect("fixture fetcher lock");
    assert!(
        seen.iter().any(|u| u.ends_with("/all-chapters/")),
        "the chapter list must come from the all-chapters route: {seen:?}"
    );
}

/// Regression: `kaliscan` nests each row's update time **inside** the chapter anchor, so the
/// anchor's own text reads `Chapter 844 hours ago` for chapter 84 — and the number parsed as
/// 844. It scrambled 99% of the numbers on the first live ingest: every value plausible, every
/// reading order wrong, and nothing failed. `chapters.number` points the parse at the label
/// element instead.
///
/// The fixture is deliberately taken from a series whose labels END in the number. A series with
/// subtitled chapters ("Chapter 135 - Bad Quality") cannot reproduce this at all, because the
/// glued-on date no longer touches the digits.
#[tokio::test]
async fn kaliscan_numbers_chapters_from_the_label_not_the_anchors_glued_text() {
    let (adapter, ctx, _) = preset_adapter("kaliscan", SiteFetcher::new("", KALISCAN_SERIES));
    let chapters = adapter
        .fetch_chapters(&ctx, "/manga/whatever")
        .await
        .expect("chapter list parses");

    let numbers: Vec<f64> = chapters.iter().map(|c| c.number).collect();
    assert_eq!(
        numbers,
        vec![84.0, 83.0, 82.1, 81.0],
        "numbers must come from the label; the anchor's glued text would give 844, 831, 82.11, 813"
    );
}

/// `kaliscan` also renders a "LATEST CHAPTERS" preview strip above the real list with the same
/// row markup. Selecting on the row class alone truncated every series to the newest three; the
/// preset names the full list's container id.
#[tokio::test]
async fn kaliscan_reads_the_full_chapter_list_not_the_preview_strip() {
    let (adapter, ctx, _) = preset_adapter("kaliscan", SiteFetcher::new("", KALISCAN_SERIES));
    let chapters = adapter
        .fetch_chapters(&ctx, "/manga/whatever")
        .await
        .expect("chapter list parses");

    assert_eq!(chapters.len(), 4, "one chapter per #chapter-list li");
}

// -----------------------------------------------------------------------------------------
// Selectors whose plausible alternative parses to nothing
// -----------------------------------------------------------------------------------------

const HADESSCANS_CATALOG: &str = include_str!("../fixtures/expansion/hadesscans-catalog.html");
const HADESSCANS_SERIES: &str = include_str!("../fixtures/expansion/hadesscans-series.html");
const MANGATOWN_SERIES: &str = include_str!("../fixtures/expansion/mangatown-series.html");

/// On `hadesscans` the chapter row *is* the anchor. `link: "self"` is what reads it; a nested
/// selector — the shape every other preset here uses — matches nothing and the list parses empty.
#[tokio::test]
async fn hadesscans_reads_a_chapter_row_that_is_itself_the_anchor() {
    let (adapter, ctx, _) = preset_adapter(
        "hadesscans",
        SiteFetcher::new(HADESSCANS_CATALOG, HADESSCANS_SERIES),
    );

    let page = adapter
        .list_catalog(&ctx, 1)
        .await
        .expect("catalogue parses");
    assert_eq!(page.items.len(), 3, "one item per article.cx-poster-card");

    let chapters = adapter
        .fetch_chapters(&ctx, "/manga/whatever/")
        .await
        .expect("chapter list parses");
    assert_eq!(chapters.len(), 3, "one chapter per a.cx-chapter-item");
    assert!(
        chapters.iter().all(|c| c.path.starts_with('/')),
        "self-linked rows still store a path: {:?}",
        chapters.iter().map(|c| &c.path).collect::<Vec<_>>()
    );
}

/// `mangatown` labels a chapter "<Series Title> 526" — no `Chapter`, `Ch.` or `#` marker at all,
/// so the number comes from `parse_chapter_number`'s bare-number fallback. Pinned because it is
/// the one site here relying on that path, and because a title carrying its own digits would
/// break it: if this test starts reading a title's number, the preset needs a different source.
#[tokio::test]
async fn mangatown_numbers_chapters_from_a_label_with_no_marker() {
    let (adapter, ctx, _) = preset_adapter("mangatown", SiteFetcher::new("", MANGATOWN_SERIES));
    let chapters = adapter
        .fetch_chapters(&ctx, "/manga/whatever/")
        .await
        .expect("chapter list parses");

    assert_eq!(chapters.len(), 3, "one chapter per ul.chapter_list li");
    assert!(
        chapters.iter().all(|c| c.number > 0.0),
        "every row parses to a real chapter number: {:?}",
        chapters.iter().map(|c| c.number).collect::<Vec<_>>()
    );
}
