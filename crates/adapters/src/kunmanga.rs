//! Custom adapter for kunmanga (`www.kunmanga.co.uk`).
//!
//! The site is a hybrid: its catalogue and series pages are Madara-shaped HTML (so the
//! [`GenericConfigAdapter`] parses them with the standard selectors), but its **chapter
//! list is served from a JSON API**, not rendered into the series page. The generic
//! `div.wp-manga-chapter` rows only appear after client-side JS calls that API, so a
//! non-JS fetch of the series page yields zero chapters (design §7, §9).
//!
//! This adapter therefore delegates catalogue/latest/series parsing to an internal
//! [`GenericConfigAdapter`] and overrides only [`fetch_chapters`](KunMangaAdapter::fetch_chapters),
//! which reads every page of
//! `/api/comics/{slug}/chapters?page={n}&per_page={N}&order=asc`:
//!
//! ```json
//! { "success": true, "data": { "chapters": [ { "chapter_num": 57,
//!   "chapter_name": "Chapter 57", "chapter_slug": "chapter-57",
//!   "updated_at": "2026-07-20T18:11:23.000000Z" } ],
//!   "total": 57, "current_page": 1, "per_page": 50, "last_page": 2 } }
//! ```
//!
//! The API GET is routed through the same injected fetch stack, so it may come back as raw
//! JSON (plain fetch) or as solver-rendered HTML wrapping the JSON in a `<pre>` block when
//! a bot-management challenge had to be solved; [`extract_json`] tolerates both.

use crate::config::AdapterConfig;
use crate::error::AdapterError;
use crate::generic::GenericConfigAdapter;
use crate::html::parse_chapter_number;
use crate::types::{
    CatalogPage, ChapterMeta, Ctx, LatestUpdate, SeriesMeta, SourceAdapter,
};
use async_trait::async_trait;
use serde::Deserialize;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// Chapters requested per API page. The API caps `per_page`, but a generous value keeps
/// the number of round-trips low; the loop still honours the API's own `last_page`.
const CHAPTERS_PER_PAGE: u32 = 100;

/// Hard safety cap on API pages walked per series, mirroring the catalogue page cap so a
/// misbehaving paginator cannot loop unbounded.
const MAX_CHAPTER_PAGES: u32 = 1000;

/// Adapter for kunmanga: Madara-shaped HTML for catalogue/series, JSON API for chapters.
pub struct KunMangaAdapter {
    /// Drives catalogue/latest/series parsing with the standard Madara selectors.
    inner: GenericConfigAdapter,
}

impl KunMangaAdapter {
    /// Build from the effective (Madara-default-merged) config used for HTML parsing.
    #[must_use]
    pub fn new(config: AdapterConfig) -> Self {
        Self {
            inner: GenericConfigAdapter::new(config),
        }
    }
}

/// The `/api/comics/{slug}/chapters` envelope.
#[derive(Debug, Deserialize)]
struct ChaptersResponse {
    data: ChaptersData,
}

#[derive(Debug, Deserialize)]
struct ChaptersData {
    #[serde(default)]
    chapters: Vec<ApiChapter>,
    #[serde(default = "one")]
    last_page: u32,
}

fn one() -> u32 {
    1
}

#[derive(Debug, Deserialize)]
struct ApiChapter {
    /// Chapter number; the API sends a JSON number but a string is tolerated.
    #[serde(default)]
    chapter_num: Option<serde_json::Value>,
    #[serde(default)]
    chapter_name: Option<String>,
    chapter_slug: String,
    #[serde(default)]
    updated_at: Option<String>,
}

impl ApiChapter {
    /// The chapter number, preferring the numeric `chapter_num`, then a string form, then
    /// the marker in `chapter_name` (`"Chapter 57"`).
    fn number(&self) -> Option<f64> {
        match &self.chapter_num {
            Some(serde_json::Value::Number(n)) => n.as_f64(),
            Some(serde_json::Value::String(s)) => parse_chapter_number(s),
            _ => None,
        }
        .or_else(|| self.chapter_name.as_deref().and_then(parse_chapter_number))
    }
}

/// Extract the last non-empty path segment of a series path (`/manga/monarch` → `monarch`).
fn series_slug(path: &str) -> Option<&str> {
    path.split('?')
        .next()
        .unwrap_or(path)
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
}

/// Unescape the five predefined XML/HTML entities (`&amp;` resolved last to avoid
/// double-unescaping). Sufficient for JSON that a solver wrapped in an HTML `<pre>` block.
fn html_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#039;", "'")
        .replace("&amp;", "&")
}

/// Parse the chapters envelope from a response body that is either raw JSON or JSON that a
/// challenge solver wrapped (and HTML-entity-escaped) inside a `<pre>` block.
fn extract_json(body: &str) -> Result<ChaptersResponse, AdapterError> {
    let trimmed = body.trim_start();
    if trimmed.starts_with('{') {
        if let Ok(v) = serde_json::from_str::<ChaptersResponse>(body.trim()) {
            return Ok(v);
        }
    }
    // Solver-wrapped: pull the JSON object out of the surrounding markup and unescape it.
    if let (Some(start), Some(end)) = (body.find('{'), body.rfind('}')) {
        if end > start {
            let slice = &body[start..=end];
            let unescaped = html_unescape(slice);
            if let Ok(v) = serde_json::from_str::<ChaptersResponse>(unescaped.trim()) {
                return Ok(v);
            }
            if let Ok(v) = serde_json::from_str::<ChaptersResponse>(slice.trim()) {
                return Ok(v);
            }
        }
    }
    Err(AdapterError::Parse(
        "kunmanga chapters API returned no parseable JSON".to_owned(),
    ))
}

#[async_trait]
impl SourceAdapter for KunMangaAdapter {
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
        let slug = series_slug(path)
            .ok_or_else(|| AdapterError::Missing(format!("kunmanga series slug in {path:?}")))?;

        let mut chapters = Vec::new();
        let mut page = 1u32;
        loop {
            let api_path = format!(
                "/api/comics/{slug}/chapters?page={page}&per_page={CHAPTERS_PER_PAGE}&order=asc"
            );
            let resp = ctx.fetch(&api_path).await?;
            let payload = extract_json(&resp.body)?;

            for ch in &payload.data.chapters {
                let Some(number) = ch.number() else { continue };
                chapters.push(ChapterMeta {
                    number,
                    title: ch
                        .chapter_name
                        .as_ref()
                        .map(|s| s.trim().to_owned())
                        .filter(|s| !s.is_empty()),
                    path: format!("/manga/{slug}/{}", ch.chapter_slug),
                    published_at: ch
                        .updated_at
                        .as_deref()
                        .and_then(|s| OffsetDateTime::parse(s.trim(), &Rfc3339).ok()),
                });
            }

            // Stop at the API's own last page; the empty-page guard prevents an unbounded
            // loop if `last_page`/`current_page` are ever absent or inconsistent.
            if payload.data.chapters.is_empty() || page >= payload.data.last_page {
                break;
            }
            page += 1;
            if page > MAX_CHAPTER_PAGES {
                break;
            }
        }
        Ok(chapters)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_slug_from_series_path() {
        assert_eq!(series_slug("/manga/monarch"), Some("monarch"));
        assert_eq!(series_slug("/manga/monarch/"), Some("monarch"));
        assert_eq!(series_slug("/manga/koun-ryusui?x=1"), Some("koun-ryusui"));
        assert_eq!(series_slug("/"), None);
        assert_eq!(series_slug(""), None);
    }

    #[test]
    fn parses_raw_json_envelope() {
        let body = r#"{"success":true,"data":{"chapters":[
            {"chapter_num":57,"chapter_name":"Chapter 57","chapter_slug":"chapter-57",
             "updated_at":"2026-07-20T18:11:23.000000Z"}],
            "total":57,"current_page":1,"per_page":50,"last_page":2}}"#;
        let env = extract_json(body).expect("raw json parses");
        assert_eq!(env.data.last_page, 2);
        assert_eq!(env.data.chapters.len(), 1);
        assert_eq!(env.data.chapters[0].number(), Some(57.0));
        assert_eq!(env.data.chapters[0].chapter_slug, "chapter-57");
    }

    #[test]
    fn parses_solver_wrapped_escaped_json() {
        // FlareSolverr returns JSON as HTML-escaped text inside a <pre> block.
        let body = concat!(
            "<html><head></head><body><pre>",
            "{&quot;success&quot;:true,&quot;data&quot;:{&quot;chapters&quot;:[",
            "{&quot;chapter_num&quot;:12,&quot;chapter_name&quot;:&quot;Chapter 12&quot;,",
            "&quot;chapter_slug&quot;:&quot;chapter-12&quot;,",
            "&quot;updated_at&quot;:&quot;2026-01-02T00:00:00.000000Z&quot;}],",
            "&quot;current_page&quot;:1,&quot;last_page&quot;:1}}",
            "</pre></body></html>"
        );
        let env = extract_json(body).expect("wrapped json parses");
        assert_eq!(env.data.chapters.len(), 1);
        assert_eq!(env.data.chapters[0].number(), Some(12.0));
    }

    #[test]
    fn chapter_number_falls_back_to_name() {
        let ch = ApiChapter {
            chapter_num: None,
            chapter_name: Some("Chapter 3.5".to_owned()),
            chapter_slug: "chapter-3-5".to_owned(),
            updated_at: None,
        };
        assert_eq!(ch.number(), Some(3.5));
    }
}
