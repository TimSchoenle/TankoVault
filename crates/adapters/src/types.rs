//! The `SourceAdapter` contract and its value types (design §7).

use crate::error::AdapterError;
use async_trait::async_trait;
use tankovault_domain::{ContentType, SeriesStatus, resolve_link};
use tankovault_fetch::{FetchRequest, FetchResponse, Fetcher};
use std::sync::Arc;
use time::OffsetDateTime;

/// Per-call context bundling the provider identity and the injected transport.
///
/// Adapters own no transport — the fetch stack is injected here, so they are testable
/// with a fake fetcher and swappable in production (design Appendix A §5).
pub struct Ctx {
    /// Provider domain root. All stored paths resolve against this.
    pub base_url: String,
    /// Provider slug (rate-limit + session-cache key).
    pub provider_slug: String,
    /// The composed fetch stack.
    pub fetcher: Arc<dyn Fetcher>,
}

impl Ctx {
    /// Fetch a page identified by a **relative** path, returning the response (with its
    /// final URL, used to relativise links found on the page).
    ///
    /// # Errors
    /// [`AdapterError`] on resolve/transport failure or a non-success status.
    pub async fn fetch(&self, relative_path: &str) -> Result<FetchResponse, AdapterError> {
        let url = resolve_link(&self.base_url, relative_path)?;
        let resp = self
            .fetcher
            .get(FetchRequest::new(url, &self.provider_slug))
            .await?;
        if !resp.is_success() {
            return Err(AdapterError::Http(resp.status));
        }
        Ok(resp)
    }
}

/// One page of the provider catalogue.
pub struct CatalogPage {
    pub items: Vec<CatalogItem>,
    pub has_next: bool,
}

/// A catalogue entry: a series' relative path + its listed title.
pub struct CatalogItem {
    pub path: String,
    pub title: String,
}

/// A "latest updates" feed entry.
pub struct LatestUpdate {
    pub path: String,
    pub title: String,
    /// Newest chapter number seen on the feed (fast-scan comparison key).
    pub latest_chapter: f64,
}

/// Full metadata for one series.
pub struct SeriesMeta {
    pub title: String,
    pub alt_titles: Vec<String>,
    pub description: Option<String>,
    pub cover_url: Option<String>,
    pub tags: Vec<String>,
    pub authors: Vec<String>,
    pub status: SeriesStatus,
    pub content_type: ContentType,
    pub release_year: Option<i32>,
}

/// One chapter entry: number, optional title, and the **relative** chapter-page link.
pub struct ChapterMeta {
    pub number: f64,
    pub title: Option<String>,
    pub path: String,
    pub published_at: Option<OffsetDateTime>,
}

/// The behavioural contract every provider satisfies (config-driven or custom).
#[async_trait]
pub trait SourceAdapter: Send + Sync {
    /// Enumerate the provider catalogue one page at a time (full scan).
    async fn list_catalog(&self, ctx: &Ctx, page: u32) -> Result<CatalogPage, AdapterError>;

    /// The provider's "latest updates" feed (fast scan).
    async fn list_latest(&self, ctx: &Ctx) -> Result<Vec<LatestUpdate>, AdapterError>;

    /// Full metadata for one series, given its RELATIVE path.
    async fn fetch_series(&self, ctx: &Ctx, path: &str) -> Result<SeriesMeta, AdapterError>;

    /// The chapter list (numbers + relative links) for one series.
    async fn fetch_chapters(&self, ctx: &Ctx, path: &str)
    -> Result<Vec<ChapterMeta>, AdapterError>;
}
