//! Custom adapter for kunmanga (`www.kunmanga.co.uk`).
//!
//! The site is a hybrid: its catalogue and series pages are Madara-shaped HTML (so the
//! [`GenericConfigAdapter`] parses them with the standard selectors), but its **chapter
//! list is served from a JSON API**, not rendered into the series page. The generic
//! `div.wp-manga-chapter` rows only appear after client-side JS calls that API, so a
//! non-JS fetch of the series page yields zero chapters (design §7, §9).
//!
//! It is also **not enumerable through its own listing**: `/manga/page/{n}` is clamped
//! server-side at page 100 while the paginator's "Next" control is always rendered, so the
//! generic catalogue walk loops forever over the same ~1.1k series and never reaches the
//! other ~87k. [`list_catalog`](KunMangaAdapter::list_catalog) is therefore overridden to
//! walk the site's sitemap shards instead — see that method.
//!
//! This adapter therefore delegates latest/series parsing to an internal
//! [`GenericConfigAdapter`] and overrides [`list_catalog`](KunMangaAdapter::list_catalog)
//! and [`fetch_chapters`](KunMangaAdapter::fetch_chapters). The latter reads every page of
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
//! JSON (plain fetch) or as solver-rendered markup wrapping the JSON when a bot-management
//! challenge had to be solved; [`crate::json::parse_json_body`] tolerates both shapes and
//! reports an unsolved challenge as such rather than as malformed JSON.

use crate::config::AdapterConfig;
use crate::error::AdapterError;
use crate::generic::GenericConfigAdapter;
use crate::html::{parse_chapter_number, relativize, unescape_entities};
use crate::json::parse_json_body;
use crate::types::{
    CatalogItem, CatalogPage, ChapterMeta, Ctx, LatestUpdate, SeriesMeta, SourceAdapter,
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

/// The sitemap index advertised by the site's `robots.txt`. It lists one entry per shard;
/// the series shards are the `sitemap-comic-{n}.xml` ones.
const SITEMAP_INDEX_PATH: &str = "/sitemap.xml";

/// Marks the sitemap-index entries that enumerate **series** pages (as opposed to the
/// `sitemap-chapter-*` and `sitemap0` shards, which we do not walk).
const COMIC_SHARD_MARKER: &str = "sitemap-comic-";

/// Names the chapters call for what it is: the same XHR the site's own front-end makes.
///
/// The endpoint is behind the same bot management as the pages, and a request for
/// `text/html` with no XHR marker is exactly what that tier scores as automation. These
/// headers cost nothing and make the plain (unsolved) fetch more likely to succeed, which is
/// the fetch that does *not* burn a solve.
const API_HEADERS: [(&str, &str); 2] = [
    ("Accept", "application/json, text/plain, */*"),
    ("X-Requested-With", "XMLHttpRequest"),
];

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
///
/// `data` is optional because the API answers a request it cannot serve — an unknown slug,
/// a series it has withdrawn — with `{"success": false, "message": …}` and no payload at
/// all. Modelling that as a required field turned every such answer into "unparseable JSON",
/// which pointed at the parser instead of at the provider's actual reply.
#[derive(Debug, Deserialize)]
struct ChaptersResponse {
    #[serde(default)]
    data: Option<ChaptersData>,
    #[serde(default)]
    success: Option<bool>,
    #[serde(default)]
    message: Option<String>,
}

impl ChaptersResponse {
    /// What the API said about a response that carried no `data`, for the error message.
    fn refusal(&self) -> String {
        match (&self.message, self.success) {
            (Some(msg), _) if !msg.trim().is_empty() => msg.trim().to_owned(),
            (_, Some(false)) => "the API reported success=false with no message".to_owned(),
            _ => "the API returned an envelope with no chapter data".to_owned(),
        }
    }
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

/// Extract every `<loc>` value from a sitemap document.
///
/// Like the chapters JSON, this tolerates both shapes the fetch stack can return: raw XML
/// from a plain fetch, and solver-rendered output when a challenge had to be solved (the
/// headless browser serves an XML *viewer* page that embeds the original document and also
/// renders an entity-escaped pretty-print of it). Scanning for literal `<loc>` picks up the
/// embedded original in the rendered case and the document itself in the raw case, and
/// deduplication makes the two views of the same document collapse to one list. The
/// unescaping fallback covers a viewer that escapes the payload everywhere.
fn sitemap_locs(body: &str) -> Vec<String> {
    let locs = scan_locs(body);
    if locs.is_empty() {
        scan_locs(&unescape_entities(body))
    } else {
        locs
    }
}

/// Collect `<loc>…</loc>` contents in document order, without duplicates.
fn scan_locs(doc: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for tail in doc.split("<loc>").skip(1) {
        let Some(value) = tail.split("</loc>").next() else {
            continue;
        };
        let value = value.trim();
        if !value.is_empty() && seen.insert(value.to_owned()) {
            out.push(value.to_owned());
        }
    }
    out
}

/// Turn one sitemap `<loc>` into a catalogue entry, rejecting anything that is not a
/// series landing page (`/manga/{slug}` — chapter URLs carry a further segment).
///
/// The sitemap carries no titles, so the entry gets a **provisional** title derived from
/// the slug. That is only ever used to seed a brand-new stub: the slug is itself a
/// normalised form of the real title, so it collapses to the same matching key, and the
/// per-series enrichment task then overwrites it with the title from the series page.
fn catalog_item(page_url: &str, loc: &str) -> Option<CatalogItem> {
    let path = relativize(page_url, loc);
    let mut segments = path
        .split('?')
        .next()
        .unwrap_or(&path)
        .trim_matches('/')
        .split('/');
    let (Some("manga"), Some(slug), None) = (segments.next(), segments.next(), segments.next())
    else {
        return None;
    };
    if slug.is_empty() {
        return None;
    }
    Some(CatalogItem {
        title: title_from_slug(slug),
        path: format!("/manga/{slug}"),
    })
}

/// Render a URL slug as a provisional display title: `the-frontier-base` → `The Frontier Base`.
fn title_from_slug(slug: &str) -> String {
    slug.split('-')
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[async_trait]
impl SourceAdapter for KunMangaAdapter {
    /// Enumerate the catalogue from the site's **sitemap**, one `sitemap-comic-{n}.xml`
    /// shard per `page`, rather than from the paginated HTML listing.
    ///
    /// The HTML listing at `/manga/page/{n}` cannot be used to enumerate this site: the
    /// server **clamps the page number at 100** (requesting page 101, 500 or 20000 all
    /// return the page-100 body, which still reports `Page 100 of ~6900`) while the
    /// `a[aria-label="Next"]` control is rendered unconditionally, so `has_next` never goes
    /// false. Walking it therefore both loops forever and can only ever reach ~1.1k of the
    /// site's ~88k series. The sitemap — advertised in `robots.txt` — lists every series
    /// exactly once, ~20k URLs per shard.
    ///
    /// `has_next` is derived from the shard count in the index, so it is exact and the walk
    /// terminates on the site's own data instead of on a heuristic.
    async fn list_catalog(&self, ctx: &Ctx, page: u32) -> Result<CatalogPage, AdapterError> {
        let index = ctx.fetch(SITEMAP_INDEX_PATH).await?;
        let shards: Vec<String> = sitemap_locs(&index.body)
            .into_iter()
            .filter(|loc| loc.contains(COMIC_SHARD_MARKER))
            .collect();
        if shards.is_empty() {
            return Err(AdapterError::missing(
                &format!("kunmanga series shards ({COMIC_SHARD_MARKER}*) in {SITEMAP_INDEX_PATH}"),
                &index,
            ));
        }

        // Pages are 1-based; past the last shard the catalogue is exhausted.
        let Some(shard) = usize::try_from(page)
            .ok()
            .and_then(|p| p.checked_sub(1))
            .and_then(|i| shards.get(i))
        else {
            return Ok(CatalogPage {
                items: Vec::new(),
                has_next: false,
            });
        };

        let resp = ctx.fetch(&relativize(&index.url, shard)).await?;
        let items = sitemap_locs(&resp.body)
            .iter()
            .filter_map(|loc| catalog_item(&resp.url, loc))
            .collect();
        Ok(CatalogPage {
            items,
            has_next: (page as usize) < shards.len(),
        })
    }

    async fn list_latest(&self, ctx: &Ctx) -> Result<Vec<LatestUpdate>, AdapterError> {
        self.inner.list_latest(ctx).await
    }

    async fn fetch_series(&self, ctx: &Ctx, path: &str) -> Result<SeriesMeta, AdapterError> {
        self.inner.fetch_series(ctx, path).await
    }

    /// Read every page of the chapters API for one series.
    ///
    /// Pagination terminates on the API's own `last_page`, with two independent backstops —
    /// a page that adds nothing new, and a hard page cap — because a paginator that lies
    /// about `last_page` must not be able to hold a scan task open indefinitely.
    async fn fetch_chapters(
        &self,
        ctx: &Ctx,
        path: &str,
    ) -> Result<Vec<ChapterMeta>, AdapterError> {
        let slug = series_slug(path)
            .ok_or_else(|| AdapterError::Missing(format!("kunmanga series slug in {path:?}")))?;

        // The site's front-end calls this endpoint from the series page; sending the same
        // Referer keeps the request consistent with the session that fetched that page.
        let referer = tankovault_domain::resolve_link(&ctx.base_url, &format!("/manga/{slug}"))?;
        let mut headers: Vec<(&str, &str)> = API_HEADERS.to_vec();
        headers.push(("Referer", referer.as_str()));

        let mut chapters = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut unnumbered = 0usize;
        let mut page = 1u32;
        loop {
            let api_path = format!(
                "/api/comics/{slug}/chapters?page={page}&per_page={CHAPTERS_PER_PAGE}&order=asc"
            );
            let resp = ctx.fetch_with(&api_path, &headers).await?;
            let payload: ChaptersResponse = parse_json_body("kunmanga chapters API", &resp)?;

            // A well-formed envelope carrying no payload is the API declining, not a parse
            // problem. On the first page that means this series has no chapter data at all —
            // worth surfacing with the provider's own wording; on a later page it just ends
            // the walk with what we already have.
            let Some(data) = payload.data else {
                if page == 1 {
                    return Err(AdapterError::Missing(format!(
                        "kunmanga chapters for {slug:?}: {}",
                        payload.refusal()
                    )));
                }
                tracing::warn!(
                    series = %slug,
                    page,
                    reason = %payload.refusal(),
                    "kunmanga chapters API stopped returning data mid-walk"
                );
                break;
            };

            let mut added = 0usize;
            for ch in &data.chapters {
                let Some(number) = ch.number() else {
                    unnumbered += 1;
                    continue;
                };
                // Guards against a paginator that replays a page: a duplicate would ride all
                // the way to the upsert and inflate every count on the way.
                if !seen.insert(ch.chapter_slug.clone()) {
                    continue;
                }
                added += 1;
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

            // Stop at the API's own last page; the no-progress guard covers a `last_page`
            // that is absent, inconsistent, or simply wrong.
            if added == 0 || page >= data.last_page || page >= MAX_CHAPTER_PAGES {
                if page >= MAX_CHAPTER_PAGES && page < data.last_page {
                    tracing::warn!(
                        series = %slug,
                        page,
                        last_page = data.last_page,
                        "kunmanga chapter walk stopped at the page safety cap"
                    );
                }
                break;
            }
            page += 1;
        }

        // Silent drops are how a provider's format change becomes a slow data loss instead
        // of a failure, so the count is reported even though the scan succeeds.
        if unnumbered > 0 {
            tracing::warn!(
                series = %slug,
                unnumbered,
                kept = chapters.len(),
                "kunmanga chapters skipped: no chapter number in the API row"
            );
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

    fn api_response(body: &str) -> tankovault_fetch::FetchResponse {
        tankovault_fetch::FetchResponse {
            status: 200,
            url: "https://www.kunmanga.co.uk/api/comics/monarch/chapters?page=1".to_owned(),
            headers: Vec::new(),
            body: body.to_owned(),
            from_cache: false,
        }
    }

    fn parse(body: &str) -> Result<ChaptersResponse, AdapterError> {
        parse_json_body("kunmanga chapters API", &api_response(body))
    }

    #[test]
    fn parses_raw_json_envelope() {
        let body = r#"{"success":true,"data":{"chapters":[
            {"chapter_num":57,"chapter_name":"Chapter 57","chapter_slug":"chapter-57",
             "updated_at":"2026-07-20T18:11:23.000000Z"}],
            "total":57,"current_page":1,"per_page":50,"last_page":2}}"#;
        let data = parse(body).expect("raw json parses").data.expect("payload");
        assert_eq!(data.last_page, 2);
        assert_eq!(data.chapters.len(), 1);
        assert_eq!(data.chapters[0].number(), Some(57.0));
        assert_eq!(data.chapters[0].chapter_slug, "chapter-57");
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
        let data = parse(body)
            .expect("wrapped json parses")
            .data
            .expect("payload");
        assert_eq!(data.chapters.len(), 1);
        assert_eq!(data.chapters[0].number(), Some(12.0));
    }

    #[test]
    fn an_api_refusal_parses_and_carries_its_reason() {
        // The API answers an unknown series with a payload-less envelope. That must parse —
        // reporting it as unparseable JSON hid the real message for the whole class.
        let env = parse(r#"{"success":false,"message":"Comic not found"}"#)
            .expect("refusal envelope parses");
        assert!(env.data.is_none());
        assert_eq!(env.refusal(), "Comic not found");
    }

    #[test]
    fn an_unsolved_challenge_is_not_reported_as_a_parse_failure() {
        let body = "<html><head><title>Just a moment...</title></head><body>\
                    <div class=\"cf-turnstile\"></div></body></html>";
        let err = parse(body).expect_err("a challenge page is not chapter data");
        assert!(
            matches!(err, AdapterError::Challenged { .. }),
            "expected a challenge, got {err}"
        );
        assert!(err.is_transient(), "a challenge is worth retrying");
    }

    #[test]
    fn reads_locs_from_raw_sitemap_xml() {
        let xml = concat!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
            "<urlset><url><loc>https://www.kunmanga.co.uk/manga/moonchild</loc></url>",
            "<url><loc>https://www.kunmanga.co.uk/manga/slow-dive</loc></url></urlset>"
        );
        assert_eq!(
            sitemap_locs(xml),
            vec![
                "https://www.kunmanga.co.uk/manga/moonchild",
                "https://www.kunmanga.co.uk/manga/slow-dive"
            ]
        );
    }

    #[test]
    fn reads_locs_from_solver_rendered_xml_viewer() {
        // A solved fetch returns the browser's XML *viewer*: an escaped pretty-print plus a
        // hidden copy of the original document. Both views must collapse to one list.
        let body = concat!(
            "<html><body><div class=\"pretty-print\">",
            "&lt;loc&gt;https://www.kunmanga.co.uk/manga/moonchild&lt;/loc&gt;",
            "</div><div id=\"webkit-xml-viewer-source-xml\">",
            "<urlset><url><loc>https://www.kunmanga.co.uk/manga/moonchild</loc></url></urlset>",
            "</div></body></html>"
        );
        assert_eq!(
            sitemap_locs(body),
            vec!["https://www.kunmanga.co.uk/manga/moonchild"]
        );
    }

    #[test]
    fn reads_locs_when_every_view_is_escaped() {
        let body = "<pre>&lt;loc&gt;https://www.kunmanga.co.uk/manga/slow-dive&lt;/loc&gt;</pre>";
        assert_eq!(
            sitemap_locs(body),
            vec!["https://www.kunmanga.co.uk/manga/slow-dive"]
        );
    }

    #[test]
    fn catalog_item_keeps_series_pages_and_rejects_the_rest() {
        let page = "https://www.kunmanga.co.uk/sitemap-comic-1.xml";

        let item = catalog_item(page, "https://www.kunmanga.co.uk/manga/slow-dive")
            .expect("series page accepted");
        assert_eq!(item.path, "/manga/slow-dive");
        assert_eq!(item.title, "Slow Dive");

        // Chapter URLs carry a further segment; other sections are not series at all.
        assert!(
            catalog_item(page, "https://www.kunmanga.co.uk/manga/slow-dive/chapter-3").is_none()
        );
        assert!(catalog_item(page, "https://www.kunmanga.co.uk/manga").is_none());
        assert!(catalog_item(page, "https://www.kunmanga.co.uk/manga-genre/action").is_none());
    }

    #[test]
    fn provisional_title_normalises_to_the_real_one() {
        // The stub title is slug-derived, so it must collapse to the same matching key as
        // the site's real title — otherwise the enrichment task would create a second
        // canonical series instead of attaching to this stub.
        let slug =
            "the-frontier-unexpectedly-became-the-world-s-strongest-and-most-comfortable-base";
        let real =
            "The Frontier Unexpectedly Became the World's Strongest and Most Comfortable Base";
        assert_eq!(
            tankovault_domain::normalize_title(&title_from_slug(slug)),
            tankovault_domain::normalize_title(real)
        );
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
