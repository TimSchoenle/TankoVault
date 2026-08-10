//! WEBTOON (`webtoons.com`) — the one licensed source in this catalogue.
//!
//! Two things make it unlike every other provider here, and both are why it needs code rather
//! than selectors:
//!
//! 1. **There is no catalogue listing.** Browsing is by genre, and `robots.txt` disallows
//!    `/*/search`. The genre pages *are* the enumeration, so a catalogue page here is a genre.
//! 2. **Episodes paginate.** A series page shows ten at a time behind `&page=N`, so the whole
//!    list is several fetches and the config-driven adapter's one-page read would silently
//!    truncate every long-running title to its newest ten.
//!
//! Fast Pass (paid early-access) episodes are **not rendered to an anonymous visitor at all** —
//! verified against a Fast Pass title, where the logged-out page carries no lock markup and no
//! locked rows. So every episode this adapter can see is readable, and it reports them free
//! rather than inventing a lock state it has no evidence for.

use crate::error::AdapterError;
use crate::html::{
    absolutize, extract_first, parse_blocking, parse_date_label, parse_selector, relativize,
    text_of,
};
use crate::types::{
    CatalogItem, CatalogPage, ChapterAccess, ChapterMeta, Ctx, LatestUpdate, SeriesMeta,
    SourceAdapter,
};
use async_trait::async_trait;
use std::collections::HashSet;
use tankovault_domain::{ContentType, SeriesStatus};
use time::OffsetDateTime;

/// The site's English genre slugs, read off its own genre navigation. One catalogue page per
/// genre; a series in several genres is yielded several times, which the ingest deduplicates by
/// path.
const GENRES: [&str; 17] = [
    "action",
    "comedy",
    "drama",
    "fantasy",
    "graphic_novel",
    "heartwarming",
    "historical",
    "horror",
    "mystery",
    "romance",
    "sf",
    "slice_of_life",
    "sports",
    "super_hero",
    "supernatural",
    "thriller",
    "tiptoon",
];

/// Episode-list pages fetched per series before giving up. Ten rows per page, so this reaches
/// 2 000 episodes — past the longest title on the site.
const MAX_EPISODE_PAGES: u32 = 200;

/// The WEBTOON adapter.
pub struct WebtoonsAdapter;

impl WebtoonsAdapter {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Strip any existing `page=` parameter, so appending one cannot produce a duplicate.
    fn base_series_path(path: &str) -> String {
        let (head, query) = match path.split_once('?') {
            Some((h, q)) => (h, q),
            None => return path.to_owned(),
        };
        let kept: Vec<&str> = query
            .split('&')
            .filter(|p| !p.starts_with("page="))
            .collect();
        if kept.is_empty() {
            head.to_owned()
        } else {
            format!("{head}?{}", kept.join("&"))
        }
    }
}

impl Default for WebtoonsAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SourceAdapter for WebtoonsAdapter {
    async fn list_catalog(&self, ctx: &Ctx, page: u32) -> Result<CatalogPage, AdapterError> {
        let index = usize::try_from(page.saturating_sub(1)).unwrap_or(usize::MAX);
        let Some(genre) = GENRES.get(index) else {
            return Ok(CatalogPage {
                items: Vec::new(),
                has_next: false,
            });
        };
        let resp = ctx.fetch(&format!("/en/genres/{genre}")).await?;
        // The walk ends on the genre list's own length, not on a page-content heuristic.
        let has_next = index + 1 < GENRES.len();
        parse_blocking(resp, move |root, resp| {
            let link = parse_selector("a[href*=\"/list?title_no=\"]")?;
            let mut seen = HashSet::new();
            let mut items = Vec::new();
            for anchor in root.select(&link) {
                let Some(href) = anchor.value().attr("href") else {
                    continue;
                };
                let path = relativize(&resp.url, href);
                if !seen.insert(path.clone()) {
                    continue;
                }
                // The card's own text is title plus genre chip plus like count; the title is the
                // dedicated `.subj` element.
                let title = extract_first(anchor, "p.subj")?
                    .or_else(|| extract_first(anchor, ".subj").ok().flatten())
                    .unwrap_or_else(|| text_of(anchor));
                if title.is_empty() {
                    continue;
                }
                items.push(CatalogItem { path, title });
            }
            Ok(CatalogPage { items, has_next })
        })
        .await
    }

    async fn list_latest(&self, ctx: &Ctx) -> Result<Vec<LatestUpdate>, AdapterError> {
        // The daily schedule is the site's own "what updated" surface.
        let resp = ctx.fetch("/en/dailySchedule").await?;
        parse_blocking(resp, move |root, resp| {
            let link = parse_selector("a[href*=\"/list?title_no=\"]")?;
            let mut seen = HashSet::new();
            let mut updates = Vec::new();
            for anchor in root.select(&link) {
                let Some(href) = anchor.value().attr("href") else {
                    continue;
                };
                let path = relativize(&resp.url, href);
                if !seen.insert(path.clone()) {
                    continue;
                }
                updates.push(LatestUpdate {
                    path,
                    title: extract_first(anchor, "p.subj")?.unwrap_or_default(),
                    // The schedule shows no episode number; both callers re-ingest by path.
                    latest_chapter: 0.0,
                });
            }
            Ok(updates)
        })
        .await
    }

    async fn fetch_series(&self, ctx: &Ctx, path: &str) -> Result<SeriesMeta, AdapterError> {
        let resp = ctx.fetch(path).await?;
        parse_blocking(resp, move |root, resp| {
            let title = extract_first(root, "p.subj")?
                .or_else(|| extract_first(root, "h1.subj").ok().flatten())
                .ok_or_else(|| AdapterError::missing("webtoons series title (p.subj)", resp))?;

            let cover_url =
                extract_first(root, "span.thmb img@src")?.map(|href| absolutize(&resp.url, &href));

            let authors = extract_first(root, "div.author_area")?
                .map(|raw| {
                    // The block reads `NAME ... author info`; the trailing label is not a name.
                    crate::html::split_list(raw.split("author info").next().unwrap_or(&raw))
                })
                .unwrap_or_default();

            Ok(SeriesMeta {
                title,
                alt_titles: Vec::new(),
                description: extract_first(root, "p.summary")?,
                cover_url,
                tags: extract_first(root, "h2.genre")?.into_iter().collect(),
                authors,
                // The site marks only completed titles; anything unmarked is running.
                status: extract_first(root, "span.txt_ico_completed")?
                    .map_or(SeriesStatus::Ongoing, |_| SeriesStatus::Completed),
                // Every title here is a vertical-scroll webtoon by construction.
                content_type: ContentType::Webtoon,
                release_year: None,
            })
        })
        .await
    }

    async fn fetch_chapters(
        &self,
        ctx: &Ctx,
        path: &str,
    ) -> Result<Vec<ChapterMeta>, AdapterError> {
        let base = Self::base_series_path(path);
        let joiner = if base.contains('?') { '&' } else { '?' };
        let mut chapters: Vec<ChapterMeta> = Vec::new();
        let mut seen = HashSet::new();
        let mut exhausted = false;

        for page in 1..=MAX_EPISODE_PAGES {
            let resp = ctx.fetch(&format!("{base}{joiner}page={page}")).await?;
            let now = OffsetDateTime::now_utc();
            let batch: Vec<ChapterMeta> = parse_blocking(resp, move |root, resp| {
                let row = parse_selector("li._episodeItem")?;
                let link = parse_selector("a.detail_list_link")?;
                let mut out = Vec::new();
                for el in root.select(&row) {
                    // `data-episode-no` is the site's own sequence and the only reliable number:
                    // the visible label is free text ("[Season 3] Ep. 235"), and a season prefix
                    // parses as the chapter number if read from there.
                    let Some(number) = el
                        .value()
                        .attr("data-episode-no")
                        .and_then(|n| n.parse::<f64>().ok())
                        .filter(|n| n.is_finite())
                    else {
                        continue;
                    };
                    let Some(href) = el.select(&link).next().and_then(|a| a.value().attr("href"))
                    else {
                        continue;
                    };
                    out.push(ChapterMeta {
                        number,
                        title: extract_first(el, "span.subj span")?,
                        path: relativize(&resp.url, href),
                        published_at: extract_first(el, "span.date")?
                            .and_then(|d| parse_date_label(&d, now)),
                        // See the module note: a paid episode is not rendered to an anonymous
                        // visitor, so everything visible here is readable.
                        access: ChapterAccess::Free,
                    });
                }
                Ok(out)
            })
            .await?;

            // The paginator clamps: asking past the last page re-serves the last one. Stopping
            // on "this page added nothing new" is what terminates the walk, since `has_next`
            // has no honest source here.
            let before = chapters.len();
            for chapter in batch {
                if seen.insert(chapter.path.clone()) {
                    chapters.push(chapter);
                }
            }
            if chapters.len() == before {
                exhausted = true;
                break;
            }
        }

        if !exhausted {
            tracing::warn!(
                provider = %ctx.provider_slug,
                series = %path,
                max_pages = MAX_EPISODE_PAGES,
                collected = chapters.len(),
                "webtoons episode walk hit the page safety cap; the series is truncated"
            );
        }

        Ok(chapters)
    }
}

#[cfg(test)]
mod tests {
    use super::{GENRES, WebtoonsAdapter};

    /// The episode walk appends `page=N` to a path that already carries `?title_no=…`, and the
    /// stored path can come back with a `page=` of its own. Appending a second one makes the
    /// site answer with page 1 forever, which turns the walk into an infinite no-progress loop.
    #[test]
    fn an_existing_page_parameter_is_dropped_before_paging() {
        assert_eq!(
            WebtoonsAdapter::base_series_path("/en/fantasy/tower-of-god/list?title_no=95&page=4"),
            "/en/fantasy/tower-of-god/list?title_no=95"
        );
        assert_eq!(
            WebtoonsAdapter::base_series_path("/en/fantasy/tower-of-god/list?title_no=95"),
            "/en/fantasy/tower-of-god/list?title_no=95"
        );
        assert_eq!(
            WebtoonsAdapter::base_series_path("/en/fantasy/tower-of-god/list?page=2"),
            "/en/fantasy/tower-of-god/list"
        );
        assert_eq!(
            WebtoonsAdapter::base_series_path("/en/x/y/list"),
            "/en/x/y/list"
        );
    }

    #[test]
    fn the_genre_list_is_the_catalogue_length() {
        assert_eq!(GENRES.len(), 17);
        assert!(GENRES.iter().all(|g| !g.is_empty()));
    }
}
