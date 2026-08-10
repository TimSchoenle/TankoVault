//! `HeanCMS`, the platform several scanlator sites run (here: Omega Scans).
//!
//! This is the first source in the workspace that publishes its paywall as **data** rather than
//! as a rendered lock icon: a chapter carries `price` and `free_at`, so the early-access model
//! in [`ChapterAccess`] can be filled in exactly rather than inferred. That makes it the
//! reference implementation for the other paid sources.

use crate::error::AdapterError;
use crate::json::parse_json_body;
use crate::types::{
    CatalogItem, CatalogPage, ChapterAccess, ChapterMeta, Ctx, LatestUpdate, SeriesMeta,
    SourceAdapter,
};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;
use tankovault_domain::{ContentType, SeriesStatus};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// Rows per page. The API accepts larger values but answers slowly above this.
const PAGE_SIZE: u32 = 50;

/// Chapter pages fetched per series before giving up on a feed that never ends.
const MAX_CHAPTER_PAGES: u32 = 40;

/// The paging envelope every `query` endpoint returns.
#[derive(Debug, Deserialize)]
struct Paged<T> {
    meta: Meta,
    data: Vec<T>,
}

#[derive(Debug, Deserialize)]
struct Meta {
    #[serde(default)]
    last_page: u32,
    #[serde(default)]
    current_page: u32,
}

#[derive(Debug, Deserialize)]
struct SeriesRow {
    title: String,
    series_slug: String,
}

#[derive(Debug, Deserialize)]
struct SeriesDetail {
    id: i64,
    title: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    thumbnail: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    series_type: Option<String>,
    #[serde(default)]
    author: Option<String>,
    #[serde(default)]
    studio: Option<String>,
    #[serde(default)]
    release_year: Option<Value>,
    #[serde(default)]
    alternative_names: Option<String>,
    #[serde(default)]
    tags: Vec<Tag>,
}

#[derive(Debug, Deserialize)]
struct Tag {
    name: String,
}

#[derive(Debug, Deserialize)]
struct ChapterRow {
    chapter_name: String,
    chapter_slug: String,
    #[serde(default)]
    chapter_title: Option<String>,
    /// Coins the chapter costs. `0` is free; anything above it is early access.
    #[serde(default)]
    price: i64,
    /// When the paywall lifts. Null on a paid chapter means the site has announced no date.
    #[serde(default)]
    free_at: Option<String>,
    #[serde(default)]
    created_at: Option<String>,
}

/// Map the platform's `series_type` to the medium.
fn content_type_of(series_type: Option<&str>) -> ContentType {
    match series_type.map(str::to_ascii_lowercase).as_deref() {
        Some("manga") => ContentType::Manga,
        Some("manhwa") => ContentType::Manhwa,
        Some("manhua") => ContentType::Manhua,
        Some("webtoon") => ContentType::Webtoon,
        _ => ContentType::Unknown,
    }
}

/// Read the access state a `HeanCMS` chapter row advertises.
///
/// `price > 0` is the paywall. `free_at` is when it lifts — parsed when present, and left
/// unknown when absent or unparseable, which keeps the chapter locked rather than freeing it
/// on a date nobody published.
fn access_of(row: &ChapterRow) -> ChapterAccess {
    if row.price <= 0 {
        return ChapterAccess::Free;
    }
    ChapterAccess::EarlyAccess {
        unlocks_at: row
            .free_at
            .as_deref()
            .and_then(|s| OffsetDateTime::parse(s, &Rfc3339).ok()),
    }
}

/// A HeanCMS-backed provider.
pub struct HeanCmsAdapter {
    /// API host, from the preset's `config.api` — the platform serves its JSON from a
    /// dedicated subdomain, and there is no way to derive it from the reader host.
    api: String,
}

impl HeanCmsAdapter {
    /// Build from the provider config.
    ///
    /// # Errors
    /// [`AdapterError::Config`] when `api` is missing: the adapter cannot invent the host, and
    /// failing here names the problem instead of every request 404ing later.
    pub fn new(config: &Value) -> Result<Self, AdapterError> {
        let api = config
            .get("api")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                AdapterError::Config(
                    "heancms provider config needs an `api` host, e.g. \
                     {\"api\": \"https://api.omegascans.org\"}"
                        .to_owned(),
                )
            })?
            .trim_end_matches('/')
            .to_owned();
        Ok(Self { api })
    }

    /// The series slug out of a stored `/series/{slug}` path.
    fn slug_of(series_path: &str) -> &str {
        series_path
            .trim_end_matches('/')
            .rsplit('/')
            .find(|segment| !segment.is_empty())
            .unwrap_or(series_path)
    }

    async fn detail(&self, ctx: &Ctx, slug: &str) -> Result<SeriesDetail, AdapterError> {
        let resp = ctx.fetch(&format!("{}/series/{slug}", self.api)).await?;
        parse_json_body("heancms series", &resp)
    }
}

#[async_trait]
impl SourceAdapter for HeanCmsAdapter {
    async fn list_catalog(&self, ctx: &Ctx, page: u32) -> Result<CatalogPage, AdapterError> {
        let url = format!("{}/query?page={page}&perPage={PAGE_SIZE}", self.api);
        let resp = ctx.fetch(&url).await?;
        let body: Paged<SeriesRow> = parse_json_body("heancms query", &resp)?;

        Ok(CatalogPage {
            items: body
                .data
                .into_iter()
                .map(|row| CatalogItem {
                    path: format!("/series/{}", row.series_slug),
                    title: row.title,
                })
                .collect(),
            // The envelope states the last page, so the walk ends on the site's own count.
            has_next: body.meta.current_page < body.meta.last_page,
        })
    }

    async fn list_latest(&self, ctx: &Ctx) -> Result<Vec<LatestUpdate>, AdapterError> {
        let url = format!(
            "{}/query?page=1&perPage={PAGE_SIZE}&orderBy=latest",
            self.api
        );
        let resp = ctx.fetch(&url).await?;
        let body: Paged<SeriesRow> = parse_json_body("heancms latest", &resp)?;
        Ok(body
            .data
            .into_iter()
            .map(|row| LatestUpdate {
                path: format!("/series/{}", row.series_slug),
                title: row.title,
                latest_chapter: 0.0,
            })
            .collect())
    }

    async fn fetch_series(&self, ctx: &Ctx, path: &str) -> Result<SeriesMeta, AdapterError> {
        let detail = self.detail(ctx, Self::slug_of(path)).await?;

        let mut authors = Vec::new();
        for name in [detail.author, detail.studio].into_iter().flatten() {
            let name = name.trim().to_owned();
            if !name.is_empty()
                && !authors
                    .iter()
                    .any(|a: &String| a.eq_ignore_ascii_case(&name))
            {
                authors.push(name);
            }
        }

        Ok(SeriesMeta {
            title: detail.title,
            alt_titles: detail
                .alternative_names
                .as_deref()
                .map(crate::html::split_titles)
                .unwrap_or_default(),
            description: detail.description.filter(|d| !d.trim().is_empty()),
            cover_url: detail.thumbnail,
            tags: detail.tags.into_iter().map(|t| t.name).collect(),
            authors,
            status: detail
                .status
                .as_deref()
                .map_or(SeriesStatus::Unknown, crate::html::map_status),
            content_type: content_type_of(detail.series_type.as_deref()),
            // The field arrives as a number on some rows and a string on others.
            release_year: detail.release_year.as_ref().and_then(|v| {
                v.as_i64()
                    .and_then(|n| i32::try_from(n).ok())
                    .or_else(|| v.as_str().and_then(crate::html::parse_year))
            }),
        })
    }

    async fn fetch_chapters(
        &self,
        ctx: &Ctx,
        path: &str,
    ) -> Result<Vec<ChapterMeta>, AdapterError> {
        let slug = Self::slug_of(path).to_owned();
        // The chapter endpoint is keyed by the numeric series id, which only the detail
        // document carries.
        let series_id = self.detail(ctx, &slug).await?.id;

        let mut chapters = Vec::new();
        let mut exhausted = false;
        for page in 1..=MAX_CHAPTER_PAGES {
            let url = format!(
                "{}/chapter/query?page={page}&perPage={PAGE_SIZE}&series_id={series_id}",
                self.api
            );
            let resp = ctx.fetch(&url).await?;
            let body: Paged<ChapterRow> = parse_json_body("heancms chapters", &resp)?;
            let last_page = body.meta.last_page;

            for row in &body.data {
                let Some(number) = crate::html::parse_chapter_number(&row.chapter_name) else {
                    continue;
                };
                chapters.push(ChapterMeta {
                    number,
                    title: row
                        .chapter_title
                        .as_deref()
                        .map(str::trim)
                        .filter(|t| !t.is_empty())
                        .map(str::to_owned),
                    path: format!("/series/{slug}/{}", row.chapter_slug),
                    published_at: row
                        .created_at
                        .as_deref()
                        .and_then(|s| OffsetDateTime::parse(s, &Rfc3339).ok()),
                    access: access_of(row),
                });
            }

            if page >= last_page {
                exhausted = true;
                break;
            }
        }
        if !exhausted {
            tracing::warn!(
                provider = %ctx.provider_slug,
                series = %path,
                max_pages = MAX_CHAPTER_PAGES,
                collected = chapters.len(),
                "heancms chapter query hit the page safety cap; the series is truncated"
            );
        }
        Ok(chapters)
    }
}

#[cfg(test)]
mod tests {
    use super::{ChapterRow, HeanCmsAdapter, access_of, content_type_of};
    use crate::types::ChapterAccess;
    use serde_json::json;
    use tankovault_domain::ContentType;
    use time::macros::datetime;

    fn row(price: i64, free_at: Option<&str>) -> ChapterRow {
        ChapterRow {
            chapter_name: "Chapter 1".to_owned(),
            chapter_slug: "chapter-1".to_owned(),
            chapter_title: None,
            price,
            free_at: free_at.map(str::to_owned),
            created_at: None,
        }
    }

    #[test]
    fn a_free_chapter_is_free() {
        assert_eq!(access_of(&row(0, None)), ChapterAccess::Free);
        // A `free_at` on a zero-price row is stale platform data, not a paywall; the price is
        // what gates reading, so it decides.
        assert_eq!(
            access_of(&row(0, Some("2030-01-01T00:00:00+00:00"))),
            ChapterAccess::Free
        );
    }

    #[test]
    fn a_paid_chapter_carries_its_unlock_time() {
        assert_eq!(
            access_of(&row(20, Some("2026-08-17T15:00:00+00:00"))),
            ChapterAccess::EarlyAccess {
                unlocks_at: Some(datetime!(2026-08-17 15:00 UTC))
            }
        );
    }

    /// A paid chapter with no announced date must stay locked. Reading the missing date as
    /// "unlocks now" would put a chapter the reader cannot open into their unread count — the
    /// exact failure the early-access model exists to prevent.
    #[test]
    fn a_paid_chapter_without_a_date_stays_locked() {
        assert_eq!(
            access_of(&row(20, None)),
            ChapterAccess::EarlyAccess { unlocks_at: None }
        );
        assert_eq!(
            access_of(&row(20, Some("not a date"))),
            ChapterAccess::EarlyAccess { unlocks_at: None }
        );
    }

    #[test]
    fn the_api_host_must_be_configured() {
        assert!(HeanCmsAdapter::new(&json!({})).is_err());
        assert!(HeanCmsAdapter::new(&json!({"api": "https://api.omegascans.org"})).is_ok());
    }

    #[test]
    fn series_type_classifies_the_medium() {
        assert_eq!(content_type_of(Some("Manhwa")), ContentType::Manhwa);
        assert_eq!(content_type_of(Some("Comic")), ContentType::Unknown);
    }
}
