//! Typed view of `providers.config` (JSONB) for the config-driven adapters (design §7).

use crate::error::AdapterError;
use serde::Deserialize;

/// The selector/pagination config a generic adapter reads.
#[derive(Debug, Clone, Deserialize)]
pub struct AdapterConfig {
    /// How the full scan walks the provider's catalogue.
    pub catalog: CatalogCfg,
    /// How the fast scan reads its "latest updates" feed.
    pub latest: LatestCfg,
    /// How one series page is read.
    pub series: SeriesCfg,
    /// How a series' chapter list is read.
    pub chapters: ChaptersCfg,
}

/// Catalogue enumeration (full scan).
#[derive(Debug, Clone, Deserialize)]
pub struct CatalogCfg {
    /// Path template; `{page}` is the 1-based page number and `{offset}` the row offset that
    /// page starts at (`(page - 1) * page_size`, so `{offset}` needs `page_size` set).
    pub path: String,
    /// Selector for each catalogue item container.
    pub item: String,
    /// Selector (relative to item) for the series link; `@attr` defaults to `href`.
    pub link: String,
    /// Selector (relative to item) for the series title text.
    pub title: String,
    /// Optional selector whose presence indicates a next page.
    #[serde(default)]
    pub next: Option<String>,
    /// Rows per page, for sites that paginate by offset rather than page number. Only read to
    /// expand `{offset}`.
    #[serde(default)]
    pub page_size: Option<u32>,
    /// Hard page cap. `Some(1)` is how a single-page catalogue is expressed: without it the
    /// "a page with items implies another page" fallback re-fetches that one page forever,
    /// since a site with no paginator answers every page number with the same body.
    #[serde(default)]
    pub pages: Option<u32>,
    /// `"sitemap"` reads `<loc>` URLs from an XML sitemap shard instead of selecting items from
    /// a listing page; anything else (or absent) is the ordinary HTML listing.
    ///
    /// Sites whose own listing cannot enumerate the catalogue — clamped paginators, or search
    /// as the only browse surface — advertise a sitemap in `robots.txt` that can. `item` then
    /// names the substring a `<loc>` must contain to be a series, and `title`/`link` are unused
    /// because a sitemap carries neither: the title is derived from the slug, and the
    /// per-series enrichment task replaces it with the real one.
    #[serde(default)]
    pub mode: Option<String>,
}

/// "Latest updates" feed (fast scan).
#[derive(Debug, Clone, Deserialize)]
pub struct LatestCfg {
    pub path: String,
    pub item: String,
    #[serde(default)]
    pub link: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    /// Selector (relative to item) for the newest chapter label.
    #[serde(default)]
    pub chapter: Option<String>,
}

/// How a multi-valued text field is located on a page.
///
/// Most fields are a plain CSS selector; Madara's summary block is not, because Alternative,
/// Author(s), Artist(s) and Genre(s) render as identical rows differing only in the label text —
/// matching by label alone silently mislabels those rows into `series_titles`.
///
/// `untagged`: a JSON string is [`Self::Selector`], an object is [`Self::LabelledRow`]; a
/// malformed object gives an opaque serde error, so check field names before the selector.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum TextSource {
    /// A plain selector (`@attr` supported); every non-empty match is one value.
    Selector(String),
    /// A label/value row pair, chosen by the label's text.
    LabelledRow(LabelledRowCfg),
}

/// A label/value row: find the row whose label reads `match`, then read its value cell.
#[derive(Debug, Clone, Deserialize)]
pub struct LabelledRowCfg {
    /// Selector for each candidate row.
    pub row: String,
    /// Selector (relative to the row) for the label cell.
    ///
    /// Omitted where the theme renders no separate label element and the label is simply the
    /// row's leading text (`<div class="imptdt"> Author <i>Name</i> </div>`). In that case the
    /// row's own text is matched as a **prefix**, because it also contains the value.
    #[serde(default)]
    pub label: Option<String>,
    /// The label text to match. Compared case-insensitively, ignoring surrounding whitespace
    /// and a trailing `:` — themes are inconsistent about both.
    #[serde(rename = "match")]
    pub match_label: String,
    /// Selector (relative to the row) for the value cell; text is split on `,`/`;` into values
    /// (a literal comma in a value also splits, matching `DemonicScansAdapter`).
    ///
    /// Omitted where label and value share one text node (`<li>Author(s) : Park Hae-nae</li>`).
    /// The row's own text is then used with the matched label, and any `:` after it, stripped —
    /// without that the label would be stored as part of the first value.
    #[serde(default)]
    pub value: Option<String>,
}

/// Series metadata page.
#[derive(Debug, Clone, Deserialize)]
pub struct SeriesCfg {
    pub title: String,
    #[serde(default)]
    pub desc: Option<String>,
    #[serde(default)]
    pub cover: Option<String>,
    #[serde(default)]
    pub tags: Option<String>,
    /// Publication status. Only the first value is read.
    #[serde(default)]
    pub status: Option<TextSource>,
    /// Alternative titles. See [`TextSource`] for why these are not just selectors.
    #[serde(default)]
    pub alt: Option<TextSource>,
    #[serde(default)]
    pub author: Option<TextSource>,
    #[serde(default)]
    pub artist: Option<TextSource>,
    /// Release year. Only the first value is read.
    #[serde(default)]
    pub release: Option<TextSource>,
}

/// Chapter list.
#[derive(Debug, Clone, Deserialize)]
pub struct ChaptersCfg {
    /// Selector for each chapter row container.
    pub container: String,
    /// Selector (relative to row) for the chapter link; `@attr` defaults to `href`.
    pub link: String,
    /// Where to read the number from (`"text"` = the link text). Reserved for future modes.
    #[serde(default)]
    pub number_from: Option<String>,
    /// Optional selector (relative to row) for the element carrying the chapter *label*, when
    /// the anchor's own text is not it.
    ///
    /// Needed only for one markup shape, and it corrupts data silently rather than failing:
    /// a row that nests its update time **inside** the chapter anchor renders
    /// `<a><strong>Chapter 1</strong><time>7 days ago</time></a>`, whose concatenated text is
    /// `Chapter 17 days ago` — so the number parses as 17, every row gets a plausible wrong
    /// number, and reading order is scrambled. Absent, the anchor's full text is used, which is
    /// right everywhere the date sits outside the link.
    #[serde(default)]
    pub number: Option<String>,
    /// Optional selector (relative to row) for the published date.
    #[serde(default)]
    pub date: Option<String>,
    /// Optional selector (relative to row) for the chapter's own title, where the site lists
    /// one separately from the "Chapter N" label.
    #[serde(default)]
    pub title: Option<String>,
    /// Optional path template for a chapter list served from a URL of its own rather than the
    /// series page. `{path}` expands to the series path and `{slug}` to its last non-empty
    /// segment. Absent means "the chapter rows are on the series page".
    #[serde(default)]
    pub path: Option<String>,
    /// Optional selector (relative to row) whose **presence** marks the chapter as early
    /// access. Absence of a match means free — so this must select something that appears only
    /// on locked rows, never a container that is always rendered and merely empty.
    #[serde(default)]
    pub locked: Option<String>,
    /// Optional selector (relative to row) for when a locked chapter unlocks. Read only when
    /// `locked` matched; an unparseable or missing value leaves the unlock time unknown, which
    /// keeps the chapter locked rather than silently freeing it.
    #[serde(default)]
    pub unlock: Option<String>,
}

impl AdapterConfig {
    /// Parse from a `providers.config` JSON value.
    ///
    /// # Errors
    /// [`AdapterError::Config`] if the shape does not match.
    pub fn from_value(value: &serde_json::Value) -> Result<Self, AdapterError> {
        serde_json::from_value(value.clone()).map_err(|e| AdapterError::Config(e.to_string()))
    }
}
