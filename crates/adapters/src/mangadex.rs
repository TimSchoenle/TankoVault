//! `MangaDex`, driven by its documented public API (<https://api.mangadex.org/docs/>).
//!
//! The only source here with a first-party API and a published rate policy, and the only one
//! whose records carry AniList/MAL ids — `attributes.links` feeds the enrichment matcher
//! directly instead of it having to re-derive a match from titles.
//!
//! Two hosts are in play: the provider's `base_url` is the **reader** site, so stored paths
//! resolve to something a reader can open, while every request below names the API host
//! absolutely. `resolve_link` passes an absolute URL through unchanged, which is what makes
//! that split work without a second provider row.

use crate::error::AdapterError;
use crate::json::parse_json_body;
use crate::types::{
    CatalogItem, CatalogPage, ChapterAccess, ChapterMeta, Ctx, LatestUpdate, SeriesMeta,
    SourceAdapter,
};
use async_trait::async_trait;
use serde::Deserialize;
use std::collections::BTreeMap;
use tankovault_domain::{ContentType, SeriesStatus};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// API host. Distinct from the provider's `base_url`, which is the reader site.
const API: &str = "https://api.mangadex.org";

/// Where cover files are served from; the API returns only the file name.
const COVERS: &str = "https://uploads.mangadex.org/covers";

/// Rows per catalogue page. The API's own maximum for `/manga` is 100.
const CATALOG_LIMIT: u32 = 100;

/// Rows per chapter-feed page. The API's own maximum for a feed is 500.
const FEED_LIMIT: u32 = 500;

/// Upper bound on chapter-feed pages for one series, so a feed that never reports its end
/// cannot spin. 500 × 40 = 20 000 chapters, far past the longest series on the site.
const MAX_FEED_PAGES: u32 = 40;

/// Content ratings requested. `pornographic` is deliberately absent: this workspace has an
/// adult gate, but it gates what a *reader* may see, and there is no reason to ingest a tier
/// no configuration can currently surface.
const RATINGS: &str = "&contentRating[]=safe&contentRating[]=suggestive&contentRating[]=erotica";

/// A collection response: `data` plus the paging totals.
#[derive(Debug, Deserialize)]
struct Collection<T> {
    data: Vec<T>,
    #[serde(default)]
    total: u32,
}

/// One entity: an id, its attributes, and the entities it references.
#[derive(Debug, Deserialize)]
struct Entity<A> {
    id: String,
    attributes: A,
    #[serde(default)]
    relationships: Vec<Relationship>,
}

/// A single entity response.
#[derive(Debug, Deserialize)]
struct Single<A> {
    data: Entity<A>,
}

#[derive(Debug, Deserialize)]
struct Relationship {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    attributes: Option<RelationshipAttrs>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RelationshipAttrs {
    /// Present on `author`/`artist`.
    #[serde(default)]
    name: Option<String>,
    /// Present on `cover_art`.
    #[serde(default)]
    file_name: Option<String>,
}

/// A localised string map (`{"en": "…", "ja": "…"}`).
type Localised = BTreeMap<String, String>;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MangaAttrs {
    #[serde(default)]
    title: Localised,
    #[serde(default)]
    alt_titles: Vec<Localised>,
    #[serde(default)]
    description: Localised,
    #[serde(default)]
    original_language: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    year: Option<i32>,
    #[serde(default)]
    tags: Vec<Entity<TagAttrs>>,
}

#[derive(Debug, Deserialize)]
struct TagAttrs {
    #[serde(default)]
    name: Localised,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChapterAttrs {
    #[serde(default)]
    chapter: Option<String>,
    #[serde(default)]
    title: Option<String>,
    /// Set when the chapter lives on the scanlator's own site; there is nothing to read here.
    #[serde(default)]
    external_url: Option<String>,
    #[serde(default)]
    is_unavailable: bool,
    #[serde(default)]
    publish_at: Option<String>,
}

/// Read the English value from a localised map, else any value, else `None`.
///
/// `en` is preferred rather than required: a series with no English entry still has a title,
/// and storing nothing would drop it from the catalogue entirely.
fn localised(map: &Localised) -> Option<String> {
    map.get("en")
        .or_else(|| map.values().next())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

/// Map `MangaDex`'s `originalLanguage` to this workspace's medium classification.
fn content_type_of(language: Option<&str>) -> ContentType {
    match language {
        Some("ja") => ContentType::Manga,
        Some("ko") => ContentType::Manhwa,
        Some("zh" | "zh-hk") => ContentType::Manhua,
        _ => ContentType::Unknown,
    }
}

/// The `MangaDex` adapter.
pub struct MangaDexAdapter;

impl MangaDexAdapter {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// The series UUID out of a stored `/title/{uuid}` path.
    fn id_of(series_path: &str) -> &str {
        series_path
            .trim_end_matches('/')
            .rsplit('/')
            .find(|segment| !segment.is_empty())
            .unwrap_or(series_path)
    }
}

impl Default for MangaDexAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SourceAdapter for MangaDexAdapter {
    async fn list_catalog(&self, ctx: &Ctx, page: u32) -> Result<CatalogPage, AdapterError> {
        let offset = page.saturating_sub(1).saturating_mul(CATALOG_LIMIT);
        // Ordered by creation so the walk is stable: ordering by an updated timestamp would
        // shuffle rows between pages mid-scan and skip series the walk had not reached.
        let url = format!(
            "{API}/manga?limit={CATALOG_LIMIT}&offset={offset}&order[createdAt]=asc{RATINGS}"
        );
        let resp = ctx.fetch(&url).await?;
        let page_data: Collection<Entity<MangaAttrs>> = parse_json_body("mangadex /manga", &resp)?;

        let items = page_data
            .data
            .iter()
            .filter_map(|entity| {
                Some(CatalogItem {
                    path: format!("/title/{}", entity.id),
                    title: localised(&entity.attributes.title)?,
                })
            })
            .collect();

        // The API reports the collection total, so the end of the walk is its own answer
        // rather than a "this page had rows" guess.
        Ok(CatalogPage {
            items,
            has_next: offset.saturating_add(CATALOG_LIMIT) < page_data.total,
        })
    }

    async fn list_latest(&self, ctx: &Ctx) -> Result<Vec<LatestUpdate>, AdapterError> {
        let url = format!(
            "{API}/chapter?limit=100&translatedLanguage[]=en&order[readableAt]=desc\
             &includes[]=manga{RATINGS}"
        );
        let resp = ctx.fetch(&url).await?;
        let feed: Collection<Entity<ChapterAttrs>> = parse_json_body("mangadex /chapter", &resp)?;

        // The feed is per *chapter*; the series it belongs to is a relationship. Several
        // chapters of one series collapse onto the same path, which the caller re-ingests by
        // path, so duplicates cost a repeated no-op rather than a wrong row.
        Ok(feed
            .data
            .iter()
            .filter_map(|entity| {
                let manga = entity.relationships.iter().find(|r| r.kind == "manga")?;
                Some(LatestUpdate {
                    path: format!("/title/{}", manga.id),
                    title: String::new(),
                    latest_chapter: entity
                        .attributes
                        .chapter
                        .as_deref()
                        .and_then(|c| c.parse::<f64>().ok())
                        .filter(|n| n.is_finite())
                        .unwrap_or(0.0),
                })
            })
            .collect())
    }

    async fn fetch_series(&self, ctx: &Ctx, path: &str) -> Result<SeriesMeta, AdapterError> {
        let id = Self::id_of(path);
        let url =
            format!("{API}/manga/{id}?includes[]=cover_art&includes[]=author&includes[]=artist");
        let resp = ctx.fetch(&url).await?;
        let manga: Single<MangaAttrs> = parse_json_body("mangadex /manga/{id}", &resp)?;
        let attrs = &manga.data.attributes;

        let title = localised(&attrs.title).ok_or_else(|| {
            AdapterError::missing(&format!("mangadex series title for {id}"), &resp)
        })?;

        let cover_url = manga
            .data
            .relationships
            .iter()
            .find(|r| r.kind == "cover_art")
            .and_then(|r| r.attributes.as_ref()?.file_name.as_deref())
            .map(|file| format!("{COVERS}/{id}/{file}"));

        let authors = manga
            .data
            .relationships
            .iter()
            .filter(|r| r.kind == "author" || r.kind == "artist")
            .filter_map(|r| r.attributes.as_ref()?.name.clone())
            .fold(Vec::new(), |mut acc: Vec<String>, name| {
                // One person credited as both author and artist arrives twice.
                if !acc.iter().any(|a| a.eq_ignore_ascii_case(&name)) {
                    acc.push(name);
                }
                acc
            });

        Ok(SeriesMeta {
            title,
            alt_titles: attrs.alt_titles.iter().filter_map(localised).collect(),
            description: localised(&attrs.description),
            cover_url,
            tags: attrs
                .tags
                .iter()
                .filter_map(|t| localised(&t.attributes.name))
                .collect(),
            authors,
            status: attrs
                .status
                .as_deref()
                .map_or(SeriesStatus::Unknown, crate::html::map_status),
            content_type: content_type_of(attrs.original_language.as_deref()),
            release_year: attrs.year,
        })
    }

    async fn fetch_chapters(
        &self,
        ctx: &Ctx,
        path: &str,
    ) -> Result<Vec<ChapterMeta>, AdapterError> {
        let id = Self::id_of(path);
        let mut chapters = Vec::new();
        let mut offset = 0u32;
        let mut total = 0u32;
        let mut exhausted = false;

        for _ in 0..MAX_FEED_PAGES {
            let url = format!(
                "{API}/manga/{id}/feed?limit={FEED_LIMIT}&offset={offset}\
                 &translatedLanguage[]=en&order[chapter]=asc{RATINGS}"
            );
            let resp = ctx.fetch(&url).await?;
            let feed: Collection<Entity<ChapterAttrs>> =
                parse_json_body("mangadex chapter feed", &resp)?;
            let returned = u32::try_from(feed.data.len()).unwrap_or(FEED_LIMIT);
            total = feed.total;

            for entity in feed.data {
                let attrs = entity.attributes;
                // An external chapter is hosted by the scanlator and a link to it here would
                // 404; an unavailable one is listed but not readable. Neither is a chapter
                // this catalogue can offer, and storing them inflates every unread count.
                if attrs.external_url.is_some() || attrs.is_unavailable {
                    continue;
                }
                let Some(number) = attrs
                    .chapter
                    .as_deref()
                    .and_then(|c| c.parse::<f64>().ok())
                    .filter(|n| n.is_finite())
                else {
                    // Oneshots publish with a null chapter number and cannot be ordered.
                    continue;
                };
                chapters.push(ChapterMeta {
                    number,
                    title: attrs.title.filter(|t| !t.trim().is_empty()),
                    path: format!("/chapter/{}", entity.id),
                    published_at: attrs
                        .publish_at
                        .as_deref()
                        .and_then(|s| OffsetDateTime::parse(s, &Rfc3339).ok()),
                    // MangaDex sells nothing; every listed chapter is readable.
                    access: ChapterAccess::Free,
                });
            }

            offset = offset.saturating_add(FEED_LIMIT);
            if returned < FEED_LIMIT || offset >= feed.total {
                exhausted = true;
                break;
            }
        }

        // A cap that stops a walk early has to say so. Silently returning a short list is the
        // failure mode this whole audit exists to remove: nothing errors, the chapter count
        // simply becomes wrong and stays wrong.
        if !exhausted {
            tracing::warn!(
                provider = %ctx.provider_slug,
                series = %path,
                max_pages = MAX_FEED_PAGES,
                collected = chapters.len(),
                total,
                "mangadex chapter feed hit the page safety cap; the series is truncated"
            );
        }

        Ok(chapters)
    }
}

#[cfg(test)]
mod tests {
    use super::{MangaDexAdapter, content_type_of, localised};
    use std::collections::BTreeMap;
    use tankovault_domain::ContentType;

    #[test]
    fn series_id_survives_a_trailing_slash() {
        assert_eq!(MangaDexAdapter::id_of("/title/abc-123"), "abc-123");
        assert_eq!(MangaDexAdapter::id_of("/title/abc-123/"), "abc-123");
    }

    /// A localised map without an `en` entry still has a title, and dropping it would drop the
    /// series from the catalogue rather than merely from one field.
    #[test]
    fn a_localised_map_falls_back_off_english() {
        let mut map = BTreeMap::new();
        map.insert("ja".to_owned(), "ワンピース".to_owned());
        assert_eq!(localised(&map).as_deref(), Some("ワンピース"));

        map.insert("en".to_owned(), "One Piece".to_owned());
        assert_eq!(localised(&map).as_deref(), Some("One Piece"));

        assert_eq!(localised(&BTreeMap::new()), None);
    }

    #[test]
    fn original_language_classifies_the_medium() {
        assert_eq!(content_type_of(Some("ja")), ContentType::Manga);
        assert_eq!(content_type_of(Some("ko")), ContentType::Manhwa);
        assert_eq!(content_type_of(Some("zh")), ContentType::Manhua);
        assert_eq!(content_type_of(Some("fr")), ContentType::Unknown);
        assert_eq!(content_type_of(None), ContentType::Unknown);
    }
}
