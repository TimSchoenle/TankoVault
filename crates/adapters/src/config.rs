//! Typed view of `providers.config` (JSONB) for the config-driven adapters (design §7).

use crate::error::AdapterError;
use serde::Deserialize;

/// The selector/pagination config a generic adapter reads.
#[derive(Debug, Clone, Deserialize)]
pub struct AdapterConfig {
    pub catalog: CatalogCfg,
    pub latest: LatestCfg,
    pub series: SeriesCfg,
    pub chapters: ChaptersCfg,
}

/// Catalogue enumeration (full scan).
#[derive(Debug, Clone, Deserialize)]
pub struct CatalogCfg {
    /// Path template; `{page}` is substituted with the 1-based page number.
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
    pub label: String,
    /// The label text to match. Compared case-insensitively, ignoring surrounding whitespace
    /// and a trailing `:` — themes are inconsistent about both.
    #[serde(rename = "match")]
    pub match_label: String,
    /// Selector (relative to the row) for the value cell; text is split on `,`/`;` into values
    /// (a literal comma in a value also splits, matching `DemonicScansAdapter`).
    pub value: String,
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
    #[serde(default)]
    pub status: Option<String>,
    /// Alternative titles. See [`TextSource`] for why this one is not just a selector.
    #[serde(default)]
    pub alt: Option<TextSource>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub artist: Option<String>,
    #[serde(default)]
    pub release: Option<String>,
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
    /// Optional selector (relative to row) for the published date.
    #[serde(default)]
    pub date: Option<String>,
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
