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
    #[serde(default)]
    pub alt: Option<String>,
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
