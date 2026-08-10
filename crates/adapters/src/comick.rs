//! `ComicK`, driven by its public JSON API.
//!
//! Like [`mangadex`](crate::mangadex) the reader site and the API are different hosts, and the
//! provider's `base_url` is the reader one so stored paths stay openable.
//!
//! The API keys a comic two ways: a stable `hid` used by every chapter endpoint, and a `slug`
//! used by the reader URL. Both are needed, and only the slug is recoverable from a stored
//! path — so `fetch_chapters` resolves the comic first rather than guessing.

use crate::error::AdapterError;
use crate::json::parse_json_body;
use crate::types::{
    CatalogItem, CatalogPage, ChapterAccess, ChapterMeta, Ctx, LatestUpdate, SeriesMeta,
    SourceAdapter,
};
use async_trait::async_trait;
use serde::Deserialize;
use tankovault_domain::{ContentType, SeriesStatus};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// API host. The provider's `base_url` is the reader site.
const API: &str = "https://api.comick.dev";

/// Rows per catalogue page.
///
/// Fifty, because that is the endpoint's actual ceiling — it answers
/// `400 {"message":"Limit must be at most 50"}` above it. It does so *inconsistently*, which is
/// how a value of 100 survived every earlier check: a live walk served pages one through five
/// at 100 and rejected page six, ending the catalogue at 500 series. A limit that works until
/// it does not is worse than one that never works, because nothing fails while it is being
/// tested.
const CATALOG_LIMIT: u32 = 50;

/// Chapter pages fetched per series before giving up on a feed that never ends. Doubled with
/// the page size halving, so the reachable chapter count is unchanged.
const MAX_CHAPTER_PAGES: u32 = 120;

/// Rows per chapter page. The same ceiling as the catalogue's — see [`CATALOG_LIMIT`].
const CHAPTER_LIMIT: u32 = 50;

/// A search/browse row.
#[derive(Debug, Deserialize)]
struct SearchRow {
    slug: String,
    title: String,
}

/// `GET /comic/{slug|hid}` — the reader-facing detail document.
#[derive(Debug, Deserialize)]
struct ComicDetail {
    comic: Comic,
    #[serde(default)]
    authors: Vec<Named>,
    #[serde(default)]
    artists: Vec<Named>,
}

#[derive(Debug, Deserialize)]
struct Named {
    name: String,
}

#[derive(Debug, Deserialize)]
struct Comic {
    hid: String,
    title: String,
    #[serde(default)]
    desc: Option<String>,
    /// 1 ongoing, 2 completed, 3 cancelled, 4 hiatus — the API's own numbering.
    #[serde(default)]
    status: Option<u8>,
    #[serde(default)]
    year: Option<i32>,
    #[serde(default)]
    country: Option<String>,
    #[serde(default)]
    md_titles: Vec<MdTitle>,
    #[serde(default)]
    md_comic_md_genres: Vec<GenreLink>,
    #[serde(default)]
    cover_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MdTitle {
    title: String,
}

#[derive(Debug, Deserialize)]
struct GenreLink {
    md_genres: Genre,
}

#[derive(Debug, Deserialize)]
struct Genre {
    name: String,
}

/// `GET /comic/{hid}/chapters`.
#[derive(Debug, Deserialize)]
struct ChapterPage {
    chapters: Vec<ApiChapter>,
}

#[derive(Debug, Deserialize)]
struct ApiChapter {
    hid: String,
    #[serde(default)]
    chap: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    publish_at: Option<String>,
}

/// Map the API's numeric status to this workspace's classification.
fn status_of(code: Option<u8>) -> SeriesStatus {
    match code {
        Some(1) => SeriesStatus::Ongoing,
        Some(2) => SeriesStatus::Completed,
        Some(3) => SeriesStatus::Cancelled,
        Some(4) => SeriesStatus::Hiatus,
        _ => SeriesStatus::Unknown,
    }
}

/// Map the API's origin-country code to the medium.
fn content_type_of(country: Option<&str>) -> ContentType {
    match country {
        Some("jp") => ContentType::Manga,
        Some("kr") => ContentType::Manhwa,
        Some("cn" | "hk") => ContentType::Manhua,
        _ => ContentType::Unknown,
    }
}

/// The `ComicK` adapter.
pub struct ComickAdapter;

impl ComickAdapter {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// The comic slug out of a stored `/comic/{slug}` path.
    fn slug_of(series_path: &str) -> &str {
        series_path
            .trim_end_matches('/')
            .rsplit('/')
            .find(|segment| !segment.is_empty())
            .unwrap_or(series_path)
    }

    /// Resolve the detail document, which is also how `hid` is obtained.
    async fn detail(&self, ctx: &Ctx, slug: &str) -> Result<ComicDetail, AdapterError> {
        let resp = ctx.fetch(&format!("{API}/comic/{slug}")).await?;
        parse_json_body("comick /comic/{slug}", &resp)
    }
}

impl Default for ComickAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SourceAdapter for ComickAdapter {
    async fn list_catalog(&self, ctx: &Ctx, page: u32) -> Result<CatalogPage, AdapterError> {
        // Ordered by creation, for the same reason as MangaDex: an activity ordering reshuffles
        // rows between pages while the walk is in flight.
        let url = format!("{API}/v1.0/search?page={page}&limit={CATALOG_LIMIT}&sort=created_at");
        let resp = ctx.fetch(&url).await?;
        let rows: Vec<SearchRow> = parse_json_body("comick search", &resp)?;

        let has_next = u32::try_from(rows.len()).unwrap_or(0) >= CATALOG_LIMIT;
        Ok(CatalogPage {
            items: rows
                .into_iter()
                .map(|row| CatalogItem {
                    path: format!("/comic/{}", row.slug),
                    title: row.title,
                })
                .collect(),
            has_next,
        })
    }

    async fn list_latest(&self, ctx: &Ctx) -> Result<Vec<LatestUpdate>, AdapterError> {
        let url = format!("{API}/v1.0/search?page=1&limit={CATALOG_LIMIT}&sort=uploaded");
        let resp = ctx.fetch(&url).await?;
        let rows: Vec<SearchRow> = parse_json_body("comick latest", &resp)?;
        Ok(rows
            .into_iter()
            .map(|row| LatestUpdate {
                path: format!("/comic/{}", row.slug),
                title: row.title,
                // The browse row's `last_chapter` counts every language; the fast scan only
                // compares paths, so publishing a number that disagrees with the English feed
                // would be worse than publishing none.
                latest_chapter: 0.0,
            })
            .collect())
    }

    async fn fetch_series(&self, ctx: &Ctx, path: &str) -> Result<SeriesMeta, AdapterError> {
        let detail = self.detail(ctx, Self::slug_of(path)).await?;
        let comic = detail.comic;

        let mut authors: Vec<String> = detail.authors.into_iter().map(|a| a.name).collect();
        for artist in detail.artists {
            if !authors.iter().any(|a| a.eq_ignore_ascii_case(&artist.name)) {
                authors.push(artist.name);
            }
        }

        Ok(SeriesMeta {
            title: comic.title,
            alt_titles: comic
                .md_titles
                .into_iter()
                .map(|t| t.title)
                .filter(|t| !t.trim().is_empty())
                .collect(),
            description: comic.desc.filter(|d| !d.trim().is_empty()),
            cover_url: comic.cover_url,
            tags: comic
                .md_comic_md_genres
                .into_iter()
                .map(|g| g.md_genres.name)
                .collect(),
            authors,
            status: status_of(comic.status),
            content_type: content_type_of(comic.country.as_deref()),
            release_year: comic.year,
        })
    }

    async fn fetch_chapters(
        &self,
        ctx: &Ctx,
        path: &str,
    ) -> Result<Vec<ChapterMeta>, AdapterError> {
        // `hid`, not the slug, keys the chapter endpoint; the detail document is the only place
        // it comes from, so this costs one extra request per series and cannot be skipped.
        let hid = self.detail(ctx, Self::slug_of(path)).await?.comic.hid;

        let mut chapters = Vec::new();
        let mut exhausted = false;
        for page in 1..=MAX_CHAPTER_PAGES {
            let url =
                format!("{API}/comic/{hid}/chapters?lang=en&page={page}&limit={CHAPTER_LIMIT}");
            let resp = ctx.fetch(&url).await?;
            let body: ChapterPage = parse_json_body("comick chapters", &resp)?;
            let returned = body.chapters.len();

            for raw in body.chapters {
                let Some(number) = raw
                    .chap
                    .as_deref()
                    .and_then(|c| c.parse::<f64>().ok())
                    .filter(|n| n.is_finite())
                else {
                    continue;
                };
                chapters.push(ChapterMeta {
                    number,
                    title: raw.title.filter(|t| !t.trim().is_empty()),
                    path: format!("/comic/{}/{}", Self::slug_of(path), raw.hid),
                    published_at: raw
                        .publish_at
                        .as_deref()
                        .and_then(|s| OffsetDateTime::parse(s, &Rfc3339).ok()),
                    // ComicK hosts scanlations and sells nothing.
                    access: ChapterAccess::Free,
                });
            }

            if returned < CHAPTER_LIMIT as usize {
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
                "comick chapter list hit the page safety cap; the series is truncated"
            );
        }
        Ok(chapters)
    }
}

#[cfg(test)]
mod tests {
    use super::{ComickAdapter, content_type_of, status_of};
    use tankovault_domain::{ContentType, SeriesStatus};

    #[test]
    fn slug_survives_a_trailing_slash() {
        assert_eq!(ComickAdapter::slug_of("/comic/one-piece"), "one-piece");
        assert_eq!(ComickAdapter::slug_of("/comic/one-piece/"), "one-piece");
    }

    /// The API publishes status as an integer with no accompanying label, so this mapping is
    /// the only place its meaning is recorded — an off-by-one here silently marks every
    /// ongoing series completed, which stops the scheduler refreshing them.
    #[test]
    fn numeric_status_codes_map_to_the_domain() {
        assert_eq!(status_of(Some(1)), SeriesStatus::Ongoing);
        assert_eq!(status_of(Some(2)), SeriesStatus::Completed);
        assert_eq!(status_of(Some(3)), SeriesStatus::Cancelled);
        assert_eq!(status_of(Some(4)), SeriesStatus::Hiatus);
        assert_eq!(status_of(Some(9)), SeriesStatus::Unknown);
        assert_eq!(status_of(None), SeriesStatus::Unknown);
    }

    #[test]
    fn origin_country_classifies_the_medium() {
        assert_eq!(content_type_of(Some("jp")), ContentType::Manga);
        assert_eq!(content_type_of(Some("kr")), ContentType::Manhwa);
        assert_eq!(content_type_of(Some("cn")), ContentType::Manhua);
        assert_eq!(content_type_of(None), ContentType::Unknown);
    }
}
