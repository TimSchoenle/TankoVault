//! The config-driven adapter. A Madara-like site is a one-row insert (its selectors in
//! `providers.config`); a custom site is a small struct implementing [`SourceAdapter`].

use crate::config::AdapterConfig;
use crate::error::AdapterError;
use crate::html::{
    absolutize, extract_all, extract_first, map_status, parse_chapter_number, parse_selector,
    parse_year, relativize, split_attr,
};
use crate::types::{
    CatalogItem, CatalogPage, ChapterMeta, Ctx, LatestUpdate, SeriesMeta, SourceAdapter,
};
use async_trait::async_trait;
use tankovault_domain::{ContentType, SeriesStatus};
use scraper::{ElementRef, Html};

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

#[async_trait]
impl SourceAdapter for GenericConfigAdapter {
    async fn list_catalog(&self, ctx: &Ctx, page: u32) -> Result<CatalogPage, AdapterError> {
        let path = self
            .config
            .catalog
            .path
            .replace("{page}", &page.to_string());
        let resp = ctx.fetch(&path).await?;
        let doc = Html::parse_document(&resp.body);
        let root = doc.root_element();

        let item_sel = parse_selector(&self.config.catalog.item)?;
        let mut items = Vec::new();
        for el in root.select(&item_sel) {
            let Some(path) = extract_href(el, &self.config.catalog.link, &resp.url)? else {
                continue;
            };
            let title = extract_first(el, &self.config.catalog.title)?.unwrap_or_default();
            items.push(CatalogItem { path, title });
        }

        let has_next = match &self.config.catalog.next {
            Some(next_sel) => {
                let sel = parse_selector(next_sel)?;
                root.select(&sel).next().is_some()
            }
            // Without an explicit marker, assume more pages while a page yields items;
            // the planner caps total pages so this cannot loop unbounded.
            None => !items.is_empty(),
        };

        Ok(CatalogPage { items, has_next })
    }

    async fn list_latest(&self, ctx: &Ctx) -> Result<Vec<LatestUpdate>, AdapterError> {
        let resp = ctx.fetch(&self.config.latest.path).await?;
        let doc = Html::parse_document(&resp.body);
        let root = doc.root_element();

        let item_sel = parse_selector(&self.config.latest.item)?;
        let link_spec = self.config.latest.link.as_deref().unwrap_or("a");
        let mut updates = Vec::new();
        for el in root.select(&item_sel) {
            let Some(path) = extract_href(el, link_spec, &resp.url)? else {
                continue;
            };
            let title = match &self.config.latest.title {
                Some(t) => extract_first(el, t)?.unwrap_or_default(),
                None => extract_first(el, link_spec)?.unwrap_or_default(),
            };
            let latest_chapter = match &self.config.latest.chapter {
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
    }

    async fn fetch_series(&self, ctx: &Ctx, path: &str) -> Result<SeriesMeta, AdapterError> {
        let resp = ctx.fetch(path).await?;
        let doc = Html::parse_document(&resp.body);
        let root = doc.root_element();

        let title = extract_first(root, &self.config.series.title)?
            .ok_or_else(|| AdapterError::Missing("series title".to_owned()))?;

        let description = self
            .config
            .series
            .desc
            .as_ref()
            .map(|s| extract_first(root, s))
            .transpose()?
            .flatten();

        let cover_url = self
            .config
            .series
            .cover
            .as_ref()
            .map(|s| extract_first(root, s))
            .transpose()?
            .flatten()
            // Covers are links only and may live on a separate CDN host: resolve to an
            // absolute URL (CDN hosts pass through, document-relative ones resolve).
            .map(|href| absolutize(&resp.url, &href));

        let tags = self
            .config
            .series
            .tags
            .as_ref()
            .map(|s| extract_all(root, s))
            .transpose()?
            .unwrap_or_default();

        let status = self
            .config
            .series
            .status
            .as_ref()
            .map(|s| extract_first(root, s))
            .transpose()?
            .flatten()
            .map_or(SeriesStatus::Unknown, |t| map_status(&t));

        let alt_titles = self
            .config
            .series
            .alt
            .as_ref()
            .map(|s| extract_all(root, s))
            .transpose()?
            .unwrap_or_default();

        let authors = self
            .config
            .series
            .author
            .as_ref()
            .map(|s| extract_all(root, s))
            .transpose()?
            .unwrap_or_default();
        let artists = self
            .config
            .series
            .artist
            .as_ref()
            .map(|s| extract_all(root, s))
            .transpose()?
            .unwrap_or_default();
        let authors = merge_unique(authors, artists);

        let release_year = self
            .config
            .series
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
    }

    async fn fetch_chapters(
        &self,
        ctx: &Ctx,
        path: &str,
    ) -> Result<Vec<ChapterMeta>, AdapterError> {
        let resp = ctx.fetch(path).await?;
        let doc = Html::parse_document(&resp.body);
        let root = doc.root_element();

        let container_sel = parse_selector(&self.config.chapters.container)?;
        let (link_sel_str, link_attr) = split_attr(&self.config.chapters.link);
        let link_attr = link_attr.unwrap_or("href");
        let link_sel = parse_selector(link_sel_str)?;

        let mut chapters = Vec::new();
        for el in root.select(&container_sel) {
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
                // Date formats vary wildly per provider; left unparsed for now (optional
                // metadata). A per-format parser is a documented refinement.
                published_at: None,
            });
        }
        Ok(chapters)
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
