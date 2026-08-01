//! The config-driven adapter. A Madara-like site is a one-row insert (its selectors in
//! `providers.config`); a custom site is a small struct implementing [`SourceAdapter`].

use crate::config::{AdapterConfig, TextSource};
use crate::error::AdapterError;
use crate::html::{
    absolutize, extract_all, extract_first, map_status, parse_blocking, parse_chapter_number,
    parse_selector, parse_year, relativize, split_attr, split_list,
};
use crate::types::{
    CatalogItem, CatalogPage, ChapterMeta, Ctx, LatestUpdate, SeriesMeta, SourceAdapter,
};
use async_trait::async_trait;
use scraper::ElementRef;
use tankovault_domain::{ContentType, SeriesStatus};

/// A generic adapter parameterised entirely by selectors from `providers.config`.
pub struct GenericConfigAdapter {
    config: AdapterConfig,
}

impl GenericConfigAdapter {
    /// Build from a parsed config.
    #[must_use]
    pub fn new(config: AdapterConfig) -> Self {
        Self { config }
    }
}

/// Append `extra` onto `base`, skipping case-insensitive duplicates. Used to fold an
/// "Artist" selector's results into the "Author" list without repeating a credit that
/// lists the same person under both roles.
fn merge_unique(mut base: Vec<String>, extra: Vec<String>) -> Vec<String> {
    for item in extra {
        if !base.iter().any(|b: &String| b.eq_ignore_ascii_case(&item)) {
            base.push(item);
        }
    }
    base
}

/// Extract the first link href under `root` matching `spec` (`@attr` defaults to `href`),
/// relativised against `page_url`.
fn extract_href(
    root: ElementRef<'_>,
    spec: &str,
    page_url: &str,
) -> Result<Option<String>, AdapterError> {
    let (sel_str, attr) = split_attr(spec);
    let attr = attr.unwrap_or("href");
    let sel = parse_selector(sel_str)?;
    Ok(root
        .select(&sel)
        .next()
        .and_then(|el| el.value().attr(attr))
        .map(|href| relativize(page_url, href)))
}

/// Resolve a [`TextSource`] against a parsed page.
///
/// [`TextSource::LabelledRow`] finds the row by its label since CSS cannot select on text; a
/// page missing the row yields no values, which is correct, not a failure.
fn extract_text_source(
    root: ElementRef<'_>,
    source: &TextSource,
) -> Result<Vec<String>, AdapterError> {
    match source {
        TextSource::Selector(spec) => extract_all(root, spec),
        TextSource::LabelledRow(cfg) => {
            let row_sel = parse_selector(&cfg.row)?;
            let wanted = cfg.match_label.trim();
            for row in root.select(&row_sel) {
                let Some(label) = extract_first(row, &cfg.label)? else {
                    continue;
                };
                // Themes vary on a trailing colon and case; whitespace is already collapsed.
                if !label
                    .trim_end_matches(':')
                    .trim()
                    .eq_ignore_ascii_case(wanted)
                {
                    continue;
                }
                return Ok(extract_first(row, &cfg.value)?
                    .map(|v| split_list(&v))
                    .unwrap_or_default());
            }
            Ok(Vec::new())
        }
    }
}

#[async_trait]
impl SourceAdapter for GenericConfigAdapter {
    async fn list_catalog(&self, ctx: &Ctx, page: u32) -> Result<CatalogPage, AdapterError> {
        let path = self
            .config
            .catalog
            .path
            .replace("{page}", &page.to_string());
        let resp = ctx.fetch(&path).await?;
        // Cloned because the closure must be `'static`; cheap next to the parse it guards.
        let cfg = self.config.catalog.clone();
        parse_blocking(resp, move |root, resp| {
            let page_url = &resp.url;
            let item_sel = parse_selector(&cfg.item)?;
            // Sized from the match count, not grown by doubling: an upper bound, since the
            // loop below skips entries with no link.
            let elements: Vec<_> = root.select(&item_sel).collect();
            let mut items = Vec::with_capacity(elements.len());
            for el in elements {
                let Some(path) = extract_href(el, &cfg.link, page_url)? else {
                    continue;
                };
                let title = extract_first(el, &cfg.title)?.unwrap_or_default();
                items.push(CatalogItem { path, title });
            }

            let has_next = match &cfg.next {
                Some(next_sel) => {
                    let sel = parse_selector(next_sel)?;
                    root.select(&sel).next().is_some()
                }
                // Without an explicit marker, assume more pages while a page yields items;
                // the planner caps total pages so this cannot loop unbounded.
                None => !items.is_empty(),
            };

            Ok(CatalogPage { items, has_next })
        })
        .await
    }

    async fn list_latest(&self, ctx: &Ctx) -> Result<Vec<LatestUpdate>, AdapterError> {
        let resp = ctx.fetch(&self.config.latest.path).await?;
        let cfg = self.config.latest.clone();
        parse_blocking(resp, move |root, resp| {
            let page_url = &resp.url;
            let item_sel = parse_selector(&cfg.item)?;
            let link_spec = cfg.link.as_deref().unwrap_or("a");
            let elements: Vec<_> = root.select(&item_sel).collect();
            let mut updates = Vec::with_capacity(elements.len());
            for el in elements {
                let Some(path) = extract_href(el, link_spec, page_url)? else {
                    continue;
                };
                let title = match &cfg.title {
                    Some(t) => extract_first(el, t)?.unwrap_or_default(),
                    None => extract_first(el, link_spec)?.unwrap_or_default(),
                };
                let latest_chapter = match &cfg.chapter {
                    Some(c) => extract_first(el, c)?
                        .and_then(|s| parse_chapter_number(&s))
                        .unwrap_or(0.0),
                    None => 0.0,
                };
                updates.push(LatestUpdate {
                    path,
                    title,
                    latest_chapter,
                });
            }
            Ok(updates)
        })
        .await
    }

    async fn fetch_series(&self, ctx: &Ctx, path: &str) -> Result<SeriesMeta, AdapterError> {
        let resp = ctx.fetch(path).await?;
        let cfg = self.config.series.clone();
        parse_blocking(resp, move |root, resp| {
            let title = extract_first(root, &cfg.title)?.ok_or_else(|| {
                AdapterError::missing(&format!("series title (selector {:?})", cfg.title), resp)
            })?;

            let description = cfg
                .desc
                .as_ref()
                .map(|s| extract_first(root, s))
                .transpose()?
                .flatten();

            let cover_url = cfg
                .cover
                .as_ref()
                .map(|s| extract_first(root, s))
                .transpose()?
                .flatten()
                // Covers are links only and may live on a separate CDN host: resolve to an
                // absolute URL (CDN hosts pass through, document-relative ones resolve).
                .map(|href| absolutize(&resp.url, &href));

            let tags = cfg
                .tags
                .as_ref()
                .map(|s| extract_all(root, s))
                .transpose()?
                .unwrap_or_default();

            let status = cfg
                .status
                .as_ref()
                .map(|s| extract_first(root, s))
                .transpose()?
                .flatten()
                .map_or(SeriesStatus::Unknown, |t| map_status(&t));

            let alt_titles = cfg
                .alt
                .as_ref()
                .map(|s| extract_text_source(root, s))
                .transpose()?
                .unwrap_or_default();

            let authors = cfg
                .author
                .as_ref()
                .map(|s| extract_all(root, s))
                .transpose()?
                .unwrap_or_default();
            let artists = cfg
                .artist
                .as_ref()
                .map(|s| extract_all(root, s))
                .transpose()?
                .unwrap_or_default();
            let authors = merge_unique(authors, artists);

            let release_year = cfg
                .release
                .as_ref()
                .map(|s| extract_first(root, s))
                .transpose()?
                .flatten()
                .and_then(|t| parse_year(&t));

            Ok(SeriesMeta {
                title,
                alt_titles,
                description,
                cover_url,
                tags,
                authors,
                status,
                content_type: ContentType::Unknown,
                release_year,
            })
        })
        .await
    }

    async fn fetch_chapters(
        &self,
        ctx: &Ctx,
        path: &str,
    ) -> Result<Vec<ChapterMeta>, AdapterError> {
        let resp = ctx.fetch(path).await?;
        let cfg = self.config.chapters.clone();
        parse_blocking(resp, move |root, resp| {
            let container_sel = parse_selector(&cfg.container)?;
            let (link_sel_str, link_attr) = split_attr(&cfg.link);
            let link_attr = link_attr.unwrap_or("href");
            let link_sel = parse_selector(link_sel_str)?;

            // The largest of the three: a long-running series lists thousands of chapters on
            // one page, so this is where growth by doubling actually copies something.
            let elements: Vec<_> = root.select(&container_sel).collect();
            let mut chapters = Vec::with_capacity(elements.len());
            for el in elements {
                let Some(anchor) = el.select(&link_sel).next() else {
                    continue;
                };
                let Some(href) = anchor.value().attr(link_attr) else {
                    continue;
                };
                let text: String = anchor.text().collect();
                let Some(number) = parse_chapter_number(&text) else {
                    continue;
                };
                chapters.push(ChapterMeta {
                    number,
                    title: None,
                    path: relativize(&resp.url, href),
                    // Date formats vary wildly per provider; left unparsed as optional metadata.
                    published_at: None,
                });
            }
            Ok(chapters)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::merge_unique;

    #[test]
    fn merges_artist_into_author_list() {
        let authors = vec!["Chugong".to_owned()];
        let artists = vec!["Redice Studio".to_owned()];
        assert_eq!(
            merge_unique(authors, artists),
            vec!["Chugong".to_owned(), "Redice Studio".to_owned()]
        );
    }

    #[test]
    fn drops_a_case_insensitive_duplicate_between_author_and_artist() {
        let authors = vec!["Chugong".to_owned()];
        let artists = vec!["CHUGONG".to_owned(), "Redice Studio".to_owned()];
        assert_eq!(
            merge_unique(authors, artists),
            vec!["Chugong".to_owned(), "Redice Studio".to_owned()]
        );
    }

    #[test]
    fn empty_artist_list_leaves_authors_untouched() {
        let authors = vec!["Chugong".to_owned()];
        assert_eq!(merge_unique(authors.clone(), Vec::new()), authors);
    }
}
