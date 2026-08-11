//! The config-driven adapter. A Madara-like site is a one-row insert (its selectors in
//! `providers.config`); a custom site is a small struct implementing [`SourceAdapter`].

use crate::config::{AdapterConfig, ChaptersCfg, TextSource};
use crate::error::AdapterError;
use crate::html::{
    SELF_SPEC, absolutize, extract_all, extract_first, map_status, parse_blocking,
    parse_chapter_number, parse_date_label, parse_selector, parse_year, relativize, split_attr,
    split_titles, text_of,
};
use crate::types::{
    CatalogItem, CatalogPage, ChapterAccess, ChapterMeta, Ctx, LatestUpdate, SeriesMeta,
    SourceAdapter,
};
use async_trait::async_trait;
use scraper::ElementRef;
use tankovault_domain::{ContentType, SeriesStatus};
use time::OffsetDateTime;

/// A generic adapter parameterised entirely by selectors from `providers.config`.
pub struct GenericConfigAdapter {
    config: AdapterConfig,
}

/// `catalog.mode` value selecting sitemap enumeration.
const SITEMAP_MODE: &str = "sitemap";

impl GenericConfigAdapter {
    /// Build from a parsed config.
    #[must_use]
    pub fn new(config: AdapterConfig) -> Self {
        Self { config }
    }

    /// One sitemap shard as a catalogue page.
    ///
    /// A shard carries URLs and nothing else, so each entry gets a provisional title derived
    /// from its slug. That is not a placeholder that has to be corrected later for matching to
    /// work: the slug is already a normalised form of the real title, so it collapses to the
    /// same matching key, and the per-series enrichment task then overwrites it.
    async fn sitemap_page(
        &self,
        ctx: &Ctx,
        path: &str,
        page: u32,
    ) -> Result<CatalogPage, AdapterError> {
        let resp = match ctx.fetch(path).await {
            Ok(resp) => resp,
            // Shard `n+1` not existing is how a sitemap says "that was the last one" — the
            // index is not fetched in this mode, so there is nothing else that could. Reported
            // as an error it ends the walk *and* marks the run degraded, which is a scan
            // failure logged for every provider on this mode, every time, for working normally.
            //
            // Restricted to 404 on purpose: a 403 or a 5xx on a shard is a real failure and
            // must keep surfacing, because it means part of the catalogue was not seen.
            Err(AdapterError::Http { status: 404, .. }) => {
                return Ok(CatalogPage {
                    items: Vec::new(),
                    has_next: false,
                });
            }
            Err(e) => return Err(e),
        };
        let marker = self.config.catalog.item.clone();
        let max_pages = self.config.catalog.pages;
        parse_blocking(resp, move |_, resp| {
            let mut items = Vec::new();
            let mut seen = std::collections::HashSet::new();
            for loc in sitemap_locs(&resp.body) {
                if !loc.contains(&marker) || !seen.insert(loc.clone()) {
                    continue;
                }
                let path = relativize(&resp.url, &loc);
                let title = slug_title(&path);
                items.push(CatalogItem { path, title });
            }
            let has_next = !items.is_empty() && max_pages.is_none_or(|max| page < max);
            Ok(CatalogPage { items, has_next })
        })
        .await
    }
}

/// `<loc>` values in a sitemap document.
///
/// A solver-rendered fetch returns the browser's XML *viewer* page, which embeds the document
/// twice — an entity-escaped pretty-print plus a hidden verbatim copy. Scanning for the literal
/// tag picks up the verbatim copy in that case and the document itself in the plain case; the
/// caller dedupes, so the two views collapse to one list.
fn sitemap_locs(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(start) = rest.find("<loc>") {
        let after = &rest[start + "<loc>".len()..];
        let Some(end) = after.find("</loc>") else {
            break;
        };
        let value = after[..end].trim();
        if !value.is_empty() {
            out.push(crate::html::unescape_entities(value));
        }
        rest = &after[end + "</loc>".len()..];
    }
    out
}

/// A readable provisional title from a URL slug (`/manga/2/one-piece` → `One Piece`).
fn slug_title(path: &str) -> String {
    let slug = path
        .trim_end_matches('/')
        .rsplit('/')
        .find(|segment| !segment.is_empty() && segment.chars().any(char::is_alphabetic))
        .unwrap_or(path);
    slug.split(['-', '_'])
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

/// Expand a catalogue path template for `page`.
///
/// `{page}` is the 1-based page number. `{offset}` is the row index that page starts at, for
/// sites that paginate by offset — it needs `page_size`, and expands to `0` without one rather
/// than leaving the token in the URL, since a literal `{offset}` reaching the provider is a
/// silent 404 rather than a config error anyone would see.
fn expand_page_tokens(template: &str, page: u32, page_size: Option<u32>) -> String {
    let offset = page_size
        .and_then(|size| page.checked_sub(1)?.checked_mul(size))
        .unwrap_or(0);
    template
        .replace("{page}", &page.to_string())
        .replace("{offset}", &offset.to_string())
}

/// Expand a chapter-list path template against the series path.
///
/// Three tokens: `{path}` is the series path verbatim, `{slug}` its last non-empty segment (how
/// most sites key a chapter endpoint), and `{seg:N}` the 0-based Nth segment.
///
/// `{seg:N}` exists because `{slug}` is wrong for any site whose series URL ends in a
/// human-readable name rather than its key — `WeebCentral`'s `/series/{id}/{Name}` is keyed by the
/// id, so `{slug}` builds `/series/One-Piece/full-chapter-list`, which answers 200 with an empty
/// document. That is the worst possible failure shape: no error, and every series silently
/// ingests zero chapters.
fn expand_series_tokens(template: &str, series_path: &str) -> String {
    let segments: Vec<&str> = series_path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    let slug = segments.last().copied().unwrap_or(series_path);
    let mut out = template
        .replace("{path}", series_path)
        .replace("{slug}", slug);
    for (index, segment) in segments.iter().enumerate() {
        out = out.replace(&format!("{{seg:{index}}}"), segment);
    }
    out
}

/// Read a chapter row's access state: locked when `chapters.locked` matches inside it, with the
/// unlock time taken from `chapters.unlock` where the site publishes one.
///
/// A row is free unless the lock selector actually matches, so a provider that stops rendering
/// its lock marker degrades to "everything is free" rather than to "nothing is readable".
/// The opposite default would empty every reader's unread count on a layout change.
fn extract_access(
    row: ElementRef<'_>,
    cfg: &ChaptersCfg,
    now: OffsetDateTime,
) -> Result<ChapterAccess, AdapterError> {
    let Some(locked_sel) = cfg.locked.as_deref() else {
        return Ok(ChapterAccess::Free);
    };
    let (sel_str, attr) = split_attr(locked_sel);
    let sel = parse_selector(sel_str)?;
    let matched = match attr {
        // With an `@attr` suffix the marker is an attribute's presence *and* truthiness, so a
        // `data-locked="false"` reads as unlocked instead of as a match.
        Some(name) => row.select(&sel).any(|el| {
            el.value().attr(name).is_some_and(|v| {
                !matches!(v.trim().to_ascii_lowercase().as_str(), "" | "false" | "0")
            })
        }),
        None => row.select(&sel).next().is_some(),
    };
    if !matched {
        return Ok(ChapterAccess::Free);
    }
    let unlocks_at = cfg
        .unlock
        .as_ref()
        .map(|s| extract_first(row, s))
        .transpose()?
        .flatten()
        .and_then(|label| parse_date_label(&label, now));
    Ok(ChapterAccess::EarlyAccess { unlocks_at })
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
/// relativised against `page_url`. `spec` of [`SELF_SPEC`] reads `root`'s own attribute.
fn extract_href(
    root: ElementRef<'_>,
    spec: &str,
    page_url: &str,
) -> Result<Option<String>, AdapterError> {
    let (sel_str, attr) = split_attr(spec);
    let attr = attr.unwrap_or("href");
    if sel_str == SELF_SPEC {
        return Ok(root
            .value()
            .attr(attr)
            .map(|href| relativize(page_url, href)));
    }
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
                let matches = match &cfg.label {
                    Some(label_sel) => extract_first(row, label_sel)?.is_some_and(|label| {
                        // Themes vary on a trailing colon and case; whitespace is collapsed.
                        label
                            .trim_end_matches(':')
                            .trim()
                            .eq_ignore_ascii_case(wanted)
                    }),
                    // No label element: the row's own text is `"Author Rakhyun"`, so the label
                    // can only be matched as a prefix — equality would never hit.
                    None => text_of(row)
                        .trim_start()
                        .to_ascii_lowercase()
                        .starts_with(&wanted.to_ascii_lowercase()),
                };
                if !matches {
                    continue;
                }
                // With no `value` selector, label and value share one text node: the label has
                // to be dropped or it is stored as part of the first value. Split on the
                // separator where there is one — the rendered label is rarely the configured one
                // verbatim (`Author(s) :` is matched by `Author`), so slicing at the label's
                // length is the fallback, not the rule.
                let raw = if let Some(value_sel) = &cfg.value {
                    extract_first(row, value_sel)?
                } else {
                    let text = text_of(row);
                    let value = text.split_once(':').map_or_else(
                        || text.get(wanted.len()..).unwrap_or_default(),
                        |(_, after)| after,
                    );
                    Some(value.trim().to_owned()).filter(|s| !s.is_empty())
                };
                return Ok(raw.map(|v| split_titles(&v)).unwrap_or_default());
            }
            Ok(Vec::new())
        }
    }
}

#[async_trait]
impl SourceAdapter for GenericConfigAdapter {
    async fn list_catalog(&self, ctx: &Ctx, page: u32) -> Result<CatalogPage, AdapterError> {
        let path = expand_page_tokens(
            &self.config.catalog.path,
            page,
            self.config.catalog.page_size,
        );
        if self.config.catalog.mode.as_deref() == Some(SITEMAP_MODE) {
            return self.sitemap_page(ctx, &path, page).await;
        }
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
                // A declared cap is the site's own answer and outranks every marker: a
                // single-page catalogue re-serves that page for any page number, so the
                // "next" selector and the yielded-items fallback both say "more" forever.
                _ if cfg.pages.is_some_and(|max| page >= max) => false,
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

            let first_of = |source: Option<&TextSource>| -> Result<Option<String>, AdapterError> {
                match source {
                    Some(s) => Ok(extract_text_source(root, s)?.into_iter().next()),
                    None => Ok(None),
                }
            };

            let status =
                first_of(cfg.status.as_ref())?.map_or(SeriesStatus::Unknown, |t| map_status(&t));

            let alt_titles = cfg
                .alt
                .as_ref()
                .map(|s| extract_text_source(root, s))
                .transpose()?
                .unwrap_or_default();

            let authors = cfg
                .author
                .as_ref()
                .map(|s| extract_text_source(root, s))
                .transpose()?
                .unwrap_or_default();
            let artists = cfg
                .artist
                .as_ref()
                .map(|s| extract_text_source(root, s))
                .transpose()?
                .unwrap_or_default();
            let authors = merge_unique(authors, artists);

            let release_year = first_of(cfg.release.as_ref())?.and_then(|t| parse_year(&t));

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
        let list_path = match &self.config.chapters.path {
            Some(template) => expand_series_tokens(template, path),
            None => path.to_owned(),
        };
        let resp = ctx.fetch(&list_path).await?;
        let cfg = self.config.chapters.clone();
        // Sampled once for the whole page so every relative date on it resolves against the
        // same instant — otherwise two rows reading "3 days ago" can land on different days.
        let now = OffsetDateTime::now_utc();
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
                // The row itself is the anchor on sites that render no inner link element.
                let Some(anchor) = (if link_sel_str == SELF_SPEC {
                    Some(el)
                } else {
                    el.select(&link_sel).next()
                }) else {
                    continue;
                };
                let Some(href) = anchor.value().attr(link_attr) else {
                    continue;
                };
                let text: String = anchor.text().collect();
                let Some(number) = parse_chapter_number(&text) else {
                    continue;
                };
                let published_at = cfg
                    .date
                    .as_ref()
                    .map(|s| extract_first(el, s))
                    .transpose()?
                    .flatten()
                    .and_then(|label| parse_date_label(&label, now));
                chapters.push(ChapterMeta {
                    number,
                    title: cfg
                        .title
                        .as_ref()
                        .map(|s| extract_first(el, s))
                        .transpose()?
                        .flatten(),
                    path: relativize(&resp.url, href),
                    published_at,
                    access: extract_access(el, &cfg, now)?,
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

#[cfg(test)]
mod token_tests {
    use super::{expand_page_tokens, expand_series_tokens};

    #[test]
    fn offset_paging_needs_a_page_size() {
        let t = "/search?offset={offset}&limit=32";
        assert_eq!(
            expand_page_tokens(t, 1, Some(32)),
            "/search?offset=0&limit=32"
        );
        assert_eq!(
            expand_page_tokens(t, 3, Some(32)),
            "/search?offset=64&limit=32"
        );
        // Without a page size the token still resolves — a literal `{offset}` reaching the
        // provider is a silent 404, not a config error anyone would see.
        assert_eq!(expand_page_tokens(t, 3, None), "/search?offset=0&limit=32");
    }

    /// Regression: `WeebCentral`'s chapter endpoint is keyed by the id in `/series/{id}/{Name}`,
    /// so `{slug}` — the last segment — built a URL that answers 200 with no chapters. Nothing
    /// errored; every series simply ingested zero.
    #[test]
    fn a_segment_token_addresses_the_key_and_not_the_display_name() {
        let path = "/series/01J76XY7EXQV9RE9KQ3JYE0WZ9/Hunter-X-Hunter";
        assert_eq!(
            expand_series_tokens("/series/{seg:1}/full-chapter-list", path),
            "/series/01J76XY7EXQV9RE9KQ3JYE0WZ9/full-chapter-list"
        );
        assert_eq!(
            expand_series_tokens("/series/{slug}/full-chapter-list", path),
            "/series/Hunter-X-Hunter/full-chapter-list"
        );
        assert_eq!(expand_series_tokens("{path}/x", path), format!("{path}/x"));
    }

    #[test]
    fn slug_is_the_last_segment_with_or_without_a_trailing_slash() {
        assert_eq!(
            expand_series_tokens("{slug}", "/manga/one-piece/"),
            "one-piece"
        );
        assert_eq!(
            expand_series_tokens("{slug}", "/manga/one-piece"),
            "one-piece"
        );
    }
}
