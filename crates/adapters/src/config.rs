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
/// Almost every field is a plain CSS selector. Madara's summary block is the exception it
/// exists for: `Alternative`, `Author(s)`, `Artist(s)` and `Genre(s)` render as *structurally
/// identical* `div.post-content_item` rows that differ only in the text of their heading, and
/// CSS has no way to select on text. The Madara default for `alt` was `div.summary-heading`
/// until that was found out — it matched every row's **label**, so every series ingested from
/// a Madara provider carried "Alternative", "Author(s)", "Genre(s)" and "Status" as its
/// alternative titles. Those rows land in `series_titles`, which the trigram matcher and the
/// catalogue search both read, so this was never merely cosmetic.
///
/// Deserialisation is `untagged`: a JSON string is a [`Self::Selector`], a JSON object is a
/// [`Self::LabelledRow`]. The cost of untagged is a useless serde error on a malformed object
/// ("data did not match any variant"), so check the field names below before the selector.
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
    /// Selector (relative to the row) for the value cell; `@attr` supported. The cell's text
    /// is split on `,`/`;` into separate values, as these rows are always a joined list. A
    /// value that legitimately contains a comma is split too; that trade is deliberate and
    /// matches what `DemonicScansAdapter` has always done with its own label/value rows.
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
