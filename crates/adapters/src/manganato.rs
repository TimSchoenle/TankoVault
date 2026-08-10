//! The Manganato/Mangakakalot clone family: shared selector defaults, plus the custom adapter
//! the family's chapter list forces.
//!
//! Three live domains share this markup byte for byte (`natomanga.com`, `mangakakalot.gg`,
//! `nelomanga.net`), so the selectors are a family default and each site is a config row.
//! Chapters are the exception and the reason [`ManganatoAdapter`] exists — see below.

use crate::config::AdapterConfig;
use crate::error::AdapterError;
use crate::generic::GenericConfigAdapter;
use crate::json::parse_json_body;
use crate::types::{
    CatalogPage, ChapterAccess, ChapterMeta, Ctx, LatestUpdate, SeriesMeta, SourceAdapter,
};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// The default selector set for a Manganato-family provider.
///
/// Derived from live markup (`natomanga.com`) and pinned by `tests/manganato_fixture.rs`.
#[must_use]
pub fn manganato_default_config() -> Value {
    json!({
        "catalog": {
            "path": "/manga-list/latest-manga?page={page}",
            "item": "div.list-comic-item-wrap",
            "link": "h3 a",
            "title": "h3 a",
            // No next-page marker: the paginator renders First/Last links only, both always
            // present. Falls back to "another page exists while this one yielded items", which
            // terminates here because a page past the last renders zero items.
            "next": null
        },
        "latest": {
            "path": "/manga-list/latest-manga",
            "item": "div.list-comic-item-wrap",
            "link": "h3 a",
            "title": "h3 a",
            "chapter": "a.list-story-item-wrap-chapter"
        },
        "series": {
            "title": "ul.manga-info-text li h1",
            "desc": "div#contentBox",
            "cover": "div.manga-info-pic img@src",
            "tags": "ul.manga-info-text li.genres a",
            // Author, Status, view count and last-updated render as identical `<li>` rows with
            // the label and value in one text node (`Author(s) : Park Hae-nae`). Positional
            // selectors would work until a row is added; matching the label does not.
            "status": { "row": "ul.manga-info-text li", "match": "Status" },
            "author": { "row": "ul.manga-info-text li", "match": "Author" },
            "alt": { "row": "h2.story-alternative", "match": "Alternative" }
        },
        // Present so the config parses and a series page still yields *something* if the API
        // below is unreachable; `ManganatoAdapter` overrides `fetch_chapters` and never uses it.
        "chapters": {
            "container": "div.chapter-list div.row",
            "link": "span a",
            "number_from": "text",
            "date": "span:nth-of-type(3)"
        }
    })
}

/// One chapter as the family's JSON endpoint returns it.
#[derive(Debug, Deserialize)]
struct ApiChapter {
    chapter_name: Option<String>,
    chapter_slug: String,
    chapter_num: Option<f64>,
    updated_at: Option<String>,
}

/// The envelope around it.
#[derive(Debug, Deserialize)]
struct ApiEnvelope {
    data: ApiData,
}

#[derive(Debug, Deserialize)]
struct ApiData {
    chapters: Vec<ApiChapter>,
}

/// A Manganato-family provider: family selectors for catalogue/series, JSON for chapters.
///
/// **The series page carries only the newest ~25 chapters.** The rest are fetched client-side
/// from `/api/manga/{slug}/chapters`, so a selector-only adapter silently truncates every long
/// series to its most recent page — the failure mode is not an error but a chapter count that
/// looks plausible and is wrong, which is exactly the shape of bug the `KunManga` JSON chapter API
/// produced before it was found.
pub struct ManganatoAdapter {
    inner: GenericConfigAdapter,
}

impl ManganatoAdapter {
    /// Build from an effective (family defaults + provider overrides) config.
    #[must_use]
    pub fn new(config: AdapterConfig) -> Self {
        Self {
            inner: GenericConfigAdapter::new(config),
        }
    }

    /// The series slug is the last path segment; the API is keyed by it.
    fn slug_of(series_path: &str) -> &str {
        series_path
            .rsplit('/')
            .find(|segment| !segment.is_empty())
            .unwrap_or(series_path)
    }
}

#[async_trait]
impl SourceAdapter for ManganatoAdapter {
    async fn list_catalog(&self, ctx: &Ctx, page: u32) -> Result<CatalogPage, AdapterError> {
        self.inner.list_catalog(ctx, page).await
    }

    async fn list_latest(&self, ctx: &Ctx) -> Result<Vec<LatestUpdate>, AdapterError> {
        self.inner.list_latest(ctx).await
    }

    async fn fetch_series(&self, ctx: &Ctx, path: &str) -> Result<SeriesMeta, AdapterError> {
        self.inner.fetch_series(ctx, path).await
    }

    async fn fetch_chapters(
        &self,
        ctx: &Ctx,
        path: &str,
    ) -> Result<Vec<ChapterMeta>, AdapterError> {
        let slug = Self::slug_of(path);
        let url = format!("/api/manga/{slug}/chapters");
        // The same headers the site's own front-end sends: the endpoint sits behind the same bot
        // management as the pages, and a request shaped like a document fetch is likelier to be
        // challenged than one shaped like the XHR it is.
        let resp = ctx
            .fetch_with(
                &url,
                &[
                    ("accept", "application/json"),
                    ("x-requested-with", "XMLHttpRequest"),
                ],
            )
            .await?;

        let envelope: ApiEnvelope = parse_json_body("manganato chapters API", &resp)?;

        let mut chapters = Vec::with_capacity(envelope.data.chapters.len());
        let mut unnumbered = 0usize;
        for raw in envelope.data.chapters {
            // `chapter_num` is the API's own parse of the label and is authoritative; the label
            // is only a fallback for the rows where it arrives null.
            let Some(number) = raw.chapter_num.filter(|n| n.is_finite()).or_else(|| {
                raw.chapter_name
                    .as_deref()
                    .and_then(crate::html::parse_chapter_number)
            }) else {
                unnumbered += 1;
                continue;
            };
            chapters.push(ChapterMeta {
                number,
                title: raw.chapter_name,
                path: format!("{}/{}", path.trim_end_matches('/'), raw.chapter_slug),
                published_at: raw
                    .updated_at
                    .as_deref()
                    .and_then(|s| OffsetDateTime::parse(s, &Rfc3339).ok()),
                // This family sells no early access; every listed chapter is readable.
                access: ChapterAccess::Free,
            });
        }
        if unnumbered > 0 {
            tracing::warn!(
                provider = %ctx.provider_slug,
                series = %path,
                unnumbered,
                "manganato chapters API returned rows with no usable number"
            );
        }
        Ok(chapters)
    }
}

#[cfg(test)]
mod tests {
    use super::ManganatoAdapter;

    #[test]
    fn slug_is_the_last_path_segment_with_or_without_a_trailing_slash() {
        assert_eq!(ManganatoAdapter::slug_of("/manga/one-piece"), "one-piece");
        assert_eq!(ManganatoAdapter::slug_of("/manga/one-piece/"), "one-piece");
    }
}
