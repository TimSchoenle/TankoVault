//! Iken, the platform a cluster of scanlator sites runs on (here: Vortex Scans and friends).
//!
//! The reader is a JavaScript app over a JSON API on a sibling host (`api.<domain>`), so there
//! is no server-rendered markup to select from at all — the API *is* the source. It answers with
//! more than the pages show, including the paywall flags (`isLocked`, `unlockAt`) that the
//! rendered page only expresses as a lock icon, so [`ChapterAccess`] is filled in exactly rather
//! than inferred.

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

/// Rows per query page. The API accepts more, but the payload carries every series' recent
/// chapters inline, so a larger page multiplies bytes rather than saving requests.
const PAGE_SIZE: u32 = 40;

/// The envelope `/api/query` answers with.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QueryResponse {
    #[serde(default)]
    posts: Vec<PostRow>,
    /// Series matching the query across all pages — the site's own answer to "is there a next
    /// page", which is what ends the catalogue walk.
    #[serde(default)]
    total_count: u32,
}

/// A series as the listing endpoints return it.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PostRow {
    slug: String,
    post_title: String,
    /// The platform hosts novels alongside comics under the same endpoints.
    #[serde(default)]
    is_novel: bool,
    #[serde(default)]
    series_type: Option<String>,
}

/// The envelope `/api/post` answers with.
#[derive(Debug, Deserialize)]
struct PostResponse {
    post: PostDetail,
}

/// A series as the detail endpoint returns it.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PostDetail {
    /// Numeric id — the only key `/api/chapters` accepts, and it appears nowhere but here.
    id: i64,
    post_title: String,
    /// The synopsis, as the HTML the editor stored.
    #[serde(default)]
    post_content: Option<String>,
    #[serde(default)]
    featured_image: Option<String>,
    /// Newline-separated on this platform, unlike the `;`/`|` shapes elsewhere.
    #[serde(default)]
    alternative_titles: Option<String>,
    #[serde(default)]
    author: Option<String>,
    #[serde(default)]
    artist: Option<String>,
    #[serde(default)]
    series_type: Option<String>,
    #[serde(default)]
    series_status: Option<String>,
    #[serde(default)]
    release_date: Option<String>,
    #[serde(default)]
    genres: Vec<Genre>,
}

#[derive(Debug, Deserialize)]
struct Genre {
    name: String,
}

/// The envelope `/api/chapters` answers with.
#[derive(Debug, Deserialize)]
struct ChaptersResponse {
    post: ChapterList,
}

#[derive(Debug, Deserialize)]
struct ChapterList {
    #[serde(default)]
    chapters: Vec<ChapterRow>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChapterRow {
    slug: String,
    /// A number on most rows and a string on hand-edited ones, so it is read untyped.
    number: Value,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    created_at: Option<String>,
    /// When the paywall lifts. Null on a locked row means the site announced no date.
    #[serde(default)]
    unlock_at: Option<String>,
    #[serde(default)]
    is_locked: bool,
}

/// Map the platform's `seriesType` to the medium.
fn content_type_of(series_type: Option<&str>) -> ContentType {
    match series_type.map(str::to_ascii_lowercase).as_deref() {
        Some("manga") => ContentType::Manga,
        Some("manhwa") => ContentType::Manhwa,
        Some("manhua") => ContentType::Manhua,
        Some("webtoon") => ContentType::Webtoon,
        _ => ContentType::Unknown,
    }
}

/// Map the platform's `seriesStatus` vocabulary, which [`map_status`](crate::html::map_status)
/// does not cover on its own: `COMING_SOON` and `MASS_RELEASED` are both states of a series
/// that is still releasing.
fn status_of(series_status: Option<&str>) -> SeriesStatus {
    match series_status.map(str::to_ascii_uppercase).as_deref() {
        Some("ONGOING" | "COMING_SOON" | "MASS_RELEASED") => SeriesStatus::Ongoing,
        Some("COMPLETED") => SeriesStatus::Completed,
        Some("HIATUS") => SeriesStatus::Hiatus,
        Some("CANCELLED" | "DROPPED") => SeriesStatus::Cancelled,
        _ => SeriesStatus::Unknown,
    }
}

/// Read the access state an Iken chapter row advertises.
///
/// `unlockAt` is parsed when present and left unknown when absent or unparseable, which keeps
/// the chapter locked rather than freeing it on a date nobody published.
fn access_of(row: &ChapterRow) -> ChapterAccess {
    if !row.is_locked {
        return ChapterAccess::Free;
    }
    ChapterAccess::EarlyAccess {
        unlocks_at: row
            .unlock_at
            .as_deref()
            .and_then(|s| OffsetDateTime::parse(s, &Rfc3339).ok()),
    }
}

/// Collapse the whitespace a hand-entered field arrives with.
///
/// Titles on this platform are stored exactly as typed, newlines included — and a title is
/// normalised into the matching key, so `"\n\nWhy I Quit\n"` and `"Why I Quit"` would key
/// differently and split one series across two.
fn tidy(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The chapter number, whichever way the row spells it.
fn number_of(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(crate::html::parse_number))
        .filter(|n| n.is_finite())
}

impl PostRow {
    fn into_item(self) -> CatalogItem {
        CatalogItem {
            path: series_path(&self.slug),
            title: tidy(&self.post_title),
        }
    }
}

/// The reader path for a series, which is what gets stored and what a reader opens.
fn series_path(slug: &str) -> String {
    format!("/series/{slug}")
}

/// An Iken-backed provider.
pub struct IkenAdapter {
    /// API host, from the preset's `config.api`. It is conventionally `api.` prefixed onto the
    /// reader host, but deriving it that way would bake a guess into every request; the preset
    /// states it.
    api: String,
}

impl IkenAdapter {
    /// Build from the provider config.
    ///
    /// # Errors
    /// [`AdapterError::Config`] when `api` is missing — failing here names the problem instead
    /// of every request 404ing later.
    pub fn new(config: &Value) -> Result<Self, AdapterError> {
        let api = config
            .get("api")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                AdapterError::Config(
                    "iken provider config needs an `api` host, e.g. \
                     {\"api\": \"https://api.vortexscans.org\"}"
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

    async fn detail(&self, ctx: &Ctx, slug: &str) -> Result<PostDetail, AdapterError> {
        let url = format!("{}/api/post?postSlug={slug}", self.api);
        let resp = ctx.fetch(&url).await?;
        let body: PostResponse = parse_json_body("iken series", &resp)?;
        Ok(body.post)
    }

    /// One `/api/query` page, ordered as the caller asks.
    async fn query(
        &self,
        ctx: &Ctx,
        page: u32,
        order_by: &str,
        direction: &str,
    ) -> Result<QueryResponse, AdapterError> {
        let url = format!(
            "{}/api/query?page={page}&perPage={PAGE_SIZE}&orderBy={order_by}\
             &orderDirection={direction}",
            self.api
        );
        let resp = ctx.fetch(&url).await?;
        parse_json_body("iken query", &resp)
    }
}

#[async_trait]
impl SourceAdapter for IkenAdapter {
    async fn list_catalog(&self, ctx: &Ctx, page: u32) -> Result<CatalogPage, AdapterError> {
        // Alphabetical, because the walk spans many requests and a recency order re-shuffles
        // under it: a series that gains a chapter mid-walk moves to page 1 and the entry that
        // took its place is never seen.
        let body = self.query(ctx, page, "postTitle", "asc").await?;
        // Novels share these endpoints with comics. Dropping them here rather than at ingest
        // keeps `total_count` — and therefore `has_next` — the site's own count, which is what
        // it applies to: a filtered page can legitimately be empty while more pages remain.
        let items = body
            .posts
            .into_iter()
            .filter(|row| !row.is_novel && !is_novel_type(row.series_type.as_deref()))
            .map(PostRow::into_item)
            .collect();
        Ok(CatalogPage {
            items,
            has_next: body.total_count > page * PAGE_SIZE,
        })
    }

    async fn list_latest(&self, ctx: &Ctx) -> Result<Vec<LatestUpdate>, AdapterError> {
        let body = self.query(ctx, 1, "lastChapterAddedAt", "desc").await?;
        Ok(body
            .posts
            .into_iter()
            .filter(|row| !row.is_novel && !is_novel_type(row.series_type.as_deref()))
            .map(|row| LatestUpdate {
                path: series_path(&row.slug),
                title: tidy(&row.post_title),
                // The listing carries each series' recent chapters, but both consumers of this
                // feed re-ingest by `path` and read neither the title nor this number, so
                // decoding them would be work nothing spends.
                latest_chapter: 0.0,
            })
            .collect())
    }

    async fn fetch_series(&self, ctx: &Ctx, path: &str) -> Result<SeriesMeta, AdapterError> {
        let detail = self.detail(ctx, Self::slug_of(path)).await?;

        let mut authors = Vec::new();
        for name in [detail.author, detail.artist].into_iter().flatten() {
            let name = name.trim().to_owned();
            if !name.is_empty()
                && !authors
                    .iter()
                    .any(|a: &String| a.eq_ignore_ascii_case(&name))
            {
                authors.push(name);
            }
        }

        let description = detail
            .post_content
            .as_deref()
            .map(crate::html::text_from_fragment)
            .filter(|d| !d.is_empty());

        Ok(SeriesMeta {
            title: tidy(&detail.post_title),
            alt_titles: split_alt_titles(detail.alternative_titles.as_deref()),
            description,
            cover_url: detail.featured_image.filter(|u| !u.trim().is_empty()),
            tags: detail.genres.into_iter().map(|g| g.name).collect(),
            authors,
            status: status_of(detail.series_status.as_deref()),
            content_type: content_type_of(detail.series_type.as_deref()),
            release_year: detail
                .release_date
                .as_deref()
                .and_then(crate::html::parse_year),
        })
    }

    async fn fetch_chapters(
        &self,
        ctx: &Ctx,
        path: &str,
    ) -> Result<Vec<ChapterMeta>, AdapterError> {
        let slug = Self::slug_of(path).to_owned();
        // The chapter endpoint is keyed by the numeric series id, which only the detail
        // document carries — so a chapter fetch always costs two requests.
        let series_id = self.detail(ctx, &slug).await?.id;

        let url = format!("{}/api/chapters?postId={series_id}", self.api);
        let resp = ctx.fetch(&url).await?;
        // Unpaged: the endpoint answers with the whole list, which is why there is no page cap
        // here as there is in the HeanCMS adapter.
        let body: ChaptersResponse = parse_json_body("iken chapters", &resp)?;

        Ok(body
            .post
            .chapters
            .into_iter()
            .filter_map(|row| {
                Some(ChapterMeta {
                    number: number_of(&row.number)?,
                    title: row
                        .title
                        .as_deref()
                        .map(str::trim)
                        .filter(|t| !t.is_empty())
                        .map(str::to_owned),
                    path: format!("/series/{slug}/{}", row.slug),
                    published_at: row
                        .created_at
                        .as_deref()
                        .and_then(|s| OffsetDateTime::parse(s, &Rfc3339).ok()),
                    access: access_of(&row),
                })
            })
            .collect())
    }
}

/// Whether the platform's `seriesType` names a novel. `isNovel` is the primary flag, but rows
/// predating it carry the fact only here.
fn is_novel_type(series_type: Option<&str>) -> bool {
    series_type.is_some_and(|t| t.eq_ignore_ascii_case("novel"))
}

/// Split the platform's newline-separated alternative titles, then apply the ordinary
/// separator rules to each line.
fn split_alt_titles(value: Option<&str>) -> Vec<String> {
    value
        .into_iter()
        .flat_map(|v| v.lines())
        .flat_map(crate::html::split_titles)
        .map(|t| tidy(&t))
        .filter(|t| !t.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        ChapterRow, IkenAdapter, access_of, content_type_of, number_of, split_alt_titles,
        status_of, tidy,
    };
    use crate::types::ChapterAccess;
    use serde_json::json;
    use tankovault_domain::{ContentType, SeriesStatus};
    use time::macros::datetime;

    fn row(is_locked: bool, unlock_at: Option<&str>) -> ChapterRow {
        ChapterRow {
            slug: "chapter-1".to_owned(),
            number: json!(1),
            title: None,
            created_at: None,
            unlock_at: unlock_at.map(str::to_owned),
            is_locked,
        }
    }

    #[test]
    fn an_unlocked_chapter_is_free() {
        assert_eq!(access_of(&row(false, None)), ChapterAccess::Free);
        // A stale `unlockAt` on an unlocked row is platform bookkeeping, not a paywall: the
        // flag is what gates reading, so it decides.
        assert_eq!(
            access_of(&row(false, Some("2030-01-01T00:00:00.000Z"))),
            ChapterAccess::Free
        );
    }

    #[test]
    fn a_locked_chapter_carries_its_unlock_time() {
        assert_eq!(
            access_of(&row(true, Some("2026-08-17T15:00:00.000Z"))),
            ChapterAccess::EarlyAccess {
                unlocks_at: Some(datetime!(2026-08-17 15:00 UTC))
            }
        );
    }

    /// A locked chapter with no announced date must stay locked. Reading the missing date as
    /// "unlocks now" would put a chapter the reader cannot open into their unread count — the
    /// exact failure the early-access model exists to prevent.
    #[test]
    fn a_locked_chapter_without_a_date_stays_locked() {
        assert_eq!(
            access_of(&row(true, None)),
            ChapterAccess::EarlyAccess { unlocks_at: None }
        );
        assert_eq!(
            access_of(&row(true, Some("not a date"))),
            ChapterAccess::EarlyAccess { unlocks_at: None }
        );
    }

    #[test]
    fn a_chapter_number_is_read_from_either_spelling() {
        assert_eq!(number_of(&json!(12.5)), Some(12.5));
        assert_eq!(number_of(&json!("12.5")), Some(12.5));
        assert_eq!(number_of(&json!(null)), None);
    }

    #[test]
    fn the_platform_status_vocabulary_maps_to_the_domain_one() {
        assert_eq!(status_of(Some("COMING_SOON")), SeriesStatus::Ongoing);
        assert_eq!(status_of(Some("MASS_RELEASED")), SeriesStatus::Ongoing);
        assert_eq!(status_of(Some("DROPPED")), SeriesStatus::Cancelled);
        assert_eq!(status_of(None), SeriesStatus::Unknown);
        assert_eq!(content_type_of(Some("MANHWA")), ContentType::Manhwa);
    }

    /// Regression: `postTitle` is stored as typed and several carry leading or embedded
    /// newlines. A title becomes the matching key, so an untidied one keys differently from the
    /// same title read anywhere else and splits one series into two.
    #[test]
    fn a_hand_entered_title_is_whitespace_normalised() {
        assert_eq!(
            tidy(
                "

Why I Quit Being the Demon King 
"
            ),
            "Why I Quit Being the Demon King"
        );
    }

    #[test]
    fn alternative_titles_are_split_per_line() {
        assert_eq!(
            split_alt_titles(Some("First Title\nSecond Title\n")),
            vec!["First Title".to_owned(), "Second Title".to_owned()]
        );
        assert!(split_alt_titles(None).is_empty());
    }

    #[test]
    fn the_api_host_must_be_configured() {
        assert!(IkenAdapter::new(&json!({})).is_err());
        assert!(IkenAdapter::new(&json!({"api": "https://api.vortexscans.org"})).is_ok());
    }
}
