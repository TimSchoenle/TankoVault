//! The `SourceAdapter` contract and its value types (design §7).

use crate::error::AdapterError;
use async_trait::async_trait;
use std::sync::Arc;
use tankovault_domain::{
    ChapterAccess as DomainChapterAccess, ContentType, SeriesStatus, resolve_link,
};
use tankovault_fetch::{FetchRequest, FetchResponse, Fetcher};
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
        self.fetch_with(relative_path, &[]).await
    }

    /// [`fetch`](Self::fetch) with extra request headers, for XHR-only provider APIs that
    /// content-negotiate or gate on them.
    ///
    /// # Errors
    /// [`AdapterError`] on resolve/transport failure or a non-success status.
    pub async fn fetch_with(
        &self,
        relative_path: &str,
        headers: &[(&str, &str)],
    ) -> Result<FetchResponse, AdapterError> {
        let url = resolve_link(&self.base_url, relative_path)?;
        let mut request = FetchRequest::new(url.clone(), &self.provider_slug);
        for (name, value) in headers {
            request = request.with_header(*name, *value);
        }
        let resp = self
            .fetcher
            .get(request)
            .await
            .map_err(|source| AdapterError::Fetch { url, source })?;
        if !resp.is_success() {
            return Err(AdapterError::from_response(&resp));
        }
        Ok(resp)
    }
}

/// One page of the provider catalogue.
pub struct CatalogPage {
    /// Entries in the order the page listed them. Empty is a legitimate answer for a page past
    /// the end.
    pub items: Vec<CatalogItem>,
    /// Whether a page after this one exists. The full-scan walk chains one more page task while
    /// this holds, so an adapter that cannot tell reports `false` and stops the walk rather than
    /// re-ingesting the catalogue forever.
    pub has_next: bool,
}

/// A catalogue entry: a series' relative path + its listed title.
pub struct CatalogItem {
    /// Relative to [`Ctx::base_url`]. The scan fans out one series task per path.
    pub path: String,
    /// The title as the listing printed it. It names the source stub the catalogue walk registers
    /// before any series page is fetched, and [`SeriesMeta::title`] replaces it at enrichment.
    pub title: String,
}

/// A "latest updates" feed entry.
pub struct LatestUpdate {
    /// Relative series path, the only field the fast scan acts on.
    pub path: String,
    /// The title as the feed printed it.
    pub title: String,
    /// Newest chapter number the feed listed, or `0.0` from an adapter whose feed states none.
    /// Nothing compares against it: the fast scan re-ingests every entry and lets ingest decide
    /// what is new.
    pub latest_chapter: f64,
}

/// Full metadata for one series.
///
/// Every text field arrives unbounded from a scraped page. `services/worker`'s `bound_series`
/// clips each one before ingest, so a mis-scraped selector costs a truncated string rather than
/// a megabyte in a trigram index.
pub struct SeriesMeta {
    /// The provider's canonical title for the series.
    pub title: String,
    /// Alternates the provider lists, romanisations and native-script titles among them. Each one
    /// becomes a searchable row.
    pub alt_titles: Vec<String>,
    /// The synopsis, absent when the page publishes none.
    pub description: Option<String>,
    /// The cover image URL, absent when the page publishes none. Adapters that read it out of
    /// HTML resolve it against the page; those reading a provider API pass the value through, so
    /// this is not guaranteed absolute.
    pub cover_url: Option<String>,
    /// Genre and theme labels as the provider spells them. Normalisation and blocklist pruning
    /// happen at ingest, not here.
    pub tags: Vec<String>,
    /// Credited names, with no separation of author from artist: providers rarely make one.
    pub authors: Vec<String>,
    /// Publication state, mapped from whatever label the provider prints.
    pub status: SeriesStatus,
    /// Manga, manhwa or manhua, or [`ContentType::Unknown`] from a provider that publishes no
    /// such label.
    pub content_type: ContentType,
    /// Year of first publication, absent from most providers.
    pub release_year: Option<i32>,
}

/// One chapter entry: number, optional title, and the **relative** chapter-page link.
pub struct ChapterMeta {
    /// The chapter number, fractional for `.5` side chapters. Always finite: a listing whose
    /// label does not parse to a finite number is skipped rather than clamped.
    pub number: f64,
    /// The chapter's own title, absent when the provider numbers chapters and names nothing.
    pub title: Option<String>,
    /// Relative to [`Ctx::base_url`], and the link stored for the reader to follow.
    pub path: String,
    /// Publication timestamp when the provider states one. A relative label ("3 days ago")
    /// resolves against a clock sampled once for the whole listing, so re-scanning the same page
    /// yields a later value; a label in no recognised shape leaves this `None` rather than
    /// guessing, because a wrong date reorders the release feed.
    pub published_at: Option<OffsetDateTime>,
    /// What the provider says about reading it right now.
    pub access: ChapterAccess,
}

/// A provider's access verdict for one chapter, as advertised on the page or in its API.
///
/// [`EarlyAccess`](Self::EarlyAccess) carries the unlock time when the provider states one.
/// `None` there is not "unlocks now" — it is "the provider showed a lock and no date", which
/// the read paths must keep treating as locked until a later scan says otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChapterAccess {
    /// Readable by anyone.
    #[default]
    Free,
    /// Behind a paywall or a timed early-access window.
    EarlyAccess {
        /// When the provider states the chapter opens. `None` means it showed a lock and no
        /// date, which is not the same as "opens now".
        unlocks_at: Option<OffsetDateTime>,
    },
}

impl ChapterAccess {
    /// The provider's verdict split into the two columns `chapters` stores it in.
    #[must_use]
    pub fn to_columns(self) -> (DomainChapterAccess, Option<OffsetDateTime>) {
        match self {
            Self::Free => (DomainChapterAccess::Free, None),
            Self::EarlyAccess { unlocks_at } => (DomainChapterAccess::EarlyAccess, unlocks_at),
        }
    }

    /// Locked from `now`'s point of view: early access whose stated unlock time has not passed
    /// (or which carries no unlock time at all).
    #[must_use]
    pub fn is_locked_at(self, now: OffsetDateTime) -> bool {
        match self {
            Self::Free => false,
            Self::EarlyAccess { unlocks_at } => unlocks_at.is_none_or(|at| at > now),
        }
    }
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
