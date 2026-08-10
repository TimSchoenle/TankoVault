//! Custom adapter for demonicscans.org's bespoke PHP layout, which
//! [`GenericConfigAdapter`](crate::GenericConfigAdapter) selectors can't express (SEO-prefixed
//! synopsis, label/value metadata rows). Selectors are pinned by the `demonicscans` fixtures.

use crate::error::AdapterError;
use crate::html::{
    absolutize, map_status, parse_blocking, parse_chapter_number, parse_selector, parse_ymd_date,
    relativize, split_list, split_titles, text_of,
};
use crate::types::{
    CatalogItem, CatalogPage, ChapterAccess, ChapterMeta, Ctx, LatestUpdate, SeriesMeta,
    SourceAdapter,
};
use async_trait::async_trait;
use scraper::ElementRef;
use tankovault_domain::{ContentType, SeriesStatus};

/// Adapter for the demonicscans.org custom layout.
pub struct DemonicScansAdapter;

impl DemonicScansAdapter {
    /// Construct the adapter. It is stateless — all context arrives via [`Ctx`].
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for DemonicScansAdapter {
    fn default() -> Self {
        Self::new()
    }
}

/// Read a value from the `#manga-info-stats` label/value rows, e.g. `stat_value(root,
/// "Status")` over `<div class="flex flex-row"><li>Status</li><li>Ongoing</li></div>`.
/// Returns `None` when the label is absent or its value cell is empty (`&nbsp;`).
fn stat_value(root: ElementRef<'_>, label: &str) -> Option<String> {
    let row_sel = parse_selector("#manga-info-stats div.flex-row").ok()?;
    let li_sel = parse_selector("li").ok()?;
    for row in root.select(&row_sel) {
        let mut cells = row.select(&li_sel);
        let key = cells.next().map(text_of).unwrap_or_default();
        if key.eq_ignore_ascii_case(label) {
            // `text_of` collapses `&nbsp;` to empty, so an unset value filters out here.
            return cells.next().map(text_of).filter(|v| !v.is_empty());
        }
    }
    None
}

/// Extract the synopsis from `div.white-font`, dropping the SEO boilerplate that precedes
/// the `"The Summary is"` marker when present.
fn clean_description(root: ElementRef<'_>) -> Option<String> {
    let sel = parse_selector("div.white-font").ok()?;
    let raw = root.select(&sel).next().map(text_of)?;
    let body = raw
        .split_once("The Summary is")
        .map_or(raw.as_str(), |(_, tail)| tail)
        .trim();
    (!body.is_empty()).then(|| body.to_owned())
}

#[async_trait]
impl SourceAdapter for DemonicScansAdapter {
    async fn list_catalog(&self, ctx: &Ctx, page: u32) -> Result<CatalogPage, AdapterError> {
        let resp = ctx.fetch(&format!("/advanced.php?list={page}")).await?;
        parse_blocking(resp, move |root, resp| {
            let item_sel = parse_selector("div.advanced-element")?;
            let link_sel = parse_selector("a")?;
            let mut items = Vec::new();
            for el in root.select(&item_sel) {
                let Some(anchor) = el.select(&link_sel).next() else {
                    continue;
                };
                let Some(href) = anchor.value().attr("href") else {
                    continue;
                };
                // The visible <h1> is truncated with an ellipsis; the anchor's `title`
                // attribute carries the full name, so prefer it and fall back to link text.
                let title = anchor
                    .value()
                    .attr("title")
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map_or_else(|| text_of(anchor), str::to_owned);
                items.push(CatalogItem {
                    path: relativize(&resp.url, href),
                    title,
                });
            }

            // The paginator renders an explicit "Next" anchor on every non-final page.
            let page_sel = parse_selector("div.pagination a")?;
            let has_next = root
                .select(&page_sel)
                .any(|a| text_of(a).eq_ignore_ascii_case("next"));

            Ok(CatalogPage { items, has_next })
        })
        .await
    }

    async fn list_latest(&self, ctx: &Ctx) -> Result<Vec<LatestUpdate>, AdapterError> {
        let resp = ctx.fetch("/").await?;
        parse_blocking(resp, move |root, resp| {
            let item_sel = parse_selector("div.updates-element")?;
            let link_sel = parse_selector("div.thumb a")?;
            let title_sel = parse_selector("h2 a")?;
            let chapter_sel = parse_selector("a.chplinks")?;

            let mut updates = Vec::new();
            for el in root.select(&item_sel) {
                let Some(anchor) = el.select(&link_sel).next() else {
                    continue;
                };
                let Some(href) = anchor.value().attr("href") else {
                    continue;
                };
                let title = el
                    .select(&title_sel)
                    .next()
                    .map(text_of)
                    .unwrap_or_default();
                let latest_chapter = el
                    .select(&chapter_sel)
                    .next()
                    .and_then(|c| parse_chapter_number(&text_of(c)))
                    .unwrap_or(0.0);
                updates.push(LatestUpdate {
                    path: relativize(&resp.url, href),
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
        parse_blocking(resp, move |root, resp| {
            let title_sel = parse_selector("h1.big-fat-titles")?;
            let title = root
                .select(&title_sel)
                .next()
                .map(text_of)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    AdapterError::missing("series title (selector \"h1.big-fat-titles\")", resp)
                })?;

            let cover_sel = parse_selector("#manga-page img.border-box")?;
            let cover_url = root
                .select(&cover_sel)
                .next()
                .and_then(|img| img.value().attr("src"))
                .map(|src| absolutize(&resp.url, src));

            let description = clean_description(root);

            let status =
                stat_value(root, "Status").map_or(SeriesStatus::Unknown, |s| map_status(&s));

            let alt_titles = stat_value(root, "Alternatives")
                .map(|v| split_titles(&v))
                .unwrap_or_default();

            let authors = stat_value(root, "Author")
                .map(|v| split_list(&v))
                .unwrap_or_default();

            let genre_sel = parse_selector("div.genres-list li")?;
            let tags = root
                .select(&genre_sel)
                .map(text_of)
                .filter(|s| !s.is_empty())
                .collect();

            Ok(SeriesMeta {
                title,
                alt_titles,
                description,
                cover_url,
                tags,
                authors,
                status,
                content_type: ContentType::Unknown,
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
        let resp = ctx.fetch(path).await?;
        parse_blocking(resp, move |root, resp| {
            let row_sel = parse_selector("#chapters-list li")?;
            let link_sel = parse_selector("a.chplinks")?;
            let date_sel = parse_selector("span")?;

            let mut chapters = Vec::new();
            for li in root.select(&row_sel) {
                let Some(anchor) = li.select(&link_sel).next() else {
                    continue;
                };
                let Some(href) = anchor.value().attr("href") else {
                    continue;
                };
                // The anchor text is "Chapter N …"; `parse_chapter_number` keys off the
                // "chapter" marker, so the trailing ISO date is ignored.
                let Some(number) = parse_chapter_number(&text_of(anchor)) else {
                    continue;
                };
                let published_at = anchor
                    .select(&date_sel)
                    .next()
                    .and_then(|s| parse_ymd_date(&text_of(s)));
                chapters.push(ChapterMeta {
                    number,
                    title: None,
                    path: relativize(&resp.url, href),
                    published_at,
                    // This site sells no early access; every listed chapter is readable.
                    access: ChapterAccess::Free,
                });
            }
            Ok(chapters)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::split_list;

    #[test]
    fn splits_alternatives_on_comma_and_semicolon() {
        assert_eq!(
            split_list("Shibuya Noir, 시부야 느와르; Shibuya Nowaru"),
            vec!["Shibuya Noir", "시부야 느와르", "Shibuya Nowaru"]
        );
        assert!(split_list("   ").is_empty());
        assert!(split_list("").is_empty());
    }
}
