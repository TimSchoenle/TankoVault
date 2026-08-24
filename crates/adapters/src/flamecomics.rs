//! Flame Comics — a Next.js site whose pages embed their own data as `__NEXT_DATA__`.
//!
//! The site does publish a JSON API, and its `robots.txt` disallows `/api/` for every agent.
//! So this adapter reads the *pages*, which are allowed, and takes their data from the
//! `__NEXT_DATA__` script the page ships anyway rather than from the Mantine-generated class
//! names, which carry no stable hooks.

use crate::error::AdapterError;
use crate::html::parse_blocking;
use crate::types::{
    CatalogItem, CatalogPage, ChapterAccess, ChapterMeta, Ctx, LatestUpdate, SeriesMeta,
    SourceAdapter,
};
use async_trait::async_trait;
use scraper::ElementRef;
use serde_json::Value;
use tankovault_domain::{ContentType, SeriesStatus};
use time::OffsetDateTime;

/// The embedded state Next.js writes into every page.
///
/// Returns the `props.pageProps` object, which is where this site's payload lives; the outer
/// document also carries build metadata that is of no use here.
fn page_props(root: ElementRef<'_>) -> Option<Value> {
    let selector = crate::html::parse_selector("script#__NEXT_DATA__").ok()?;
    let script = root.select(&selector).next()?;
    let raw: String = script.text().collect();
    let parsed: Value = serde_json::from_str(&raw).ok()?;
    parsed.get("props")?.get("pageProps").cloned()
}

/// Read the first present non-empty string field out of `candidates`.
fn first_str(value: &Value, candidates: &[&str]) -> Option<String> {
    candidates
        .iter()
        .find_map(|k| value.get(*k).and_then(Value::as_str))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// A Unix timestamp field as a `time` value; the site publishes seconds since the epoch.
fn epoch_seconds(value: &Value, key: &str) -> Option<OffsetDateTime> {
    let seconds = value.get(key)?.as_i64()?;
    OffsetDateTime::from_unix_timestamp(seconds).ok()
}

/// Map the site's medium vocabulary onto the domain's.
fn content_type_of(value: &Value) -> ContentType {
    match first_str(value, &["type", "series_type"])
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("manga") => ContentType::Manga,
        Some("manhwa") => ContentType::Manhwa,
        Some("manhua") => ContentType::Manhua,
        _ => ContentType::Unknown,
    }
}

/// The Flame Comics adapter.
pub struct FlameComicsAdapter;

impl FlameComicsAdapter {
    /// Stateless: the provider's base URL and the fetch stack arrive per call in [`Ctx`], so
    /// one instance serves every provider configured against this adapter.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// The numeric series id out of a stored `/series/{id}` path.
    fn id_of(series_path: &str) -> &str {
        series_path
            .trim_end_matches('/')
            .rsplit('/')
            .find(|segment| !segment.is_empty())
            .unwrap_or(series_path)
    }

    /// Series rows out of a browse page's props.
    fn series_rows(props: &Value) -> Vec<CatalogItem> {
        ["series", "allSeries", "seriesList"]
            .iter()
            .find_map(|key| props.get(*key).and_then(Value::as_array))
            .map(|rows| {
                rows.iter()
                    .filter_map(|row| {
                        let id = row.get("series_id").and_then(Value::as_i64)?;
                        Some(CatalogItem {
                            path: format!("/series/{id}"),
                            title: first_str(row, &["title", "label"])?,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl Default for FlameComicsAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SourceAdapter for FlameComicsAdapter {
    async fn list_catalog(&self, ctx: &Ctx, page: u32) -> Result<CatalogPage, AdapterError> {
        // `/browse` renders the whole catalogue into one document — the site has ~300 series and
        // no server-side paging — so page 1 is the entire catalogue and there is never a page 2.
        // Reporting `has_next` from "this page had rows" would re-ingest all of it, forever.
        if page > 1 {
            return Ok(CatalogPage {
                items: Vec::new(),
                has_next: false,
            });
        }
        let resp = ctx.fetch("/browse").await?;
        parse_blocking(resp, move |root, resp| {
            let props = page_props(root)
                .ok_or_else(|| AdapterError::missing("flamecomics __NEXT_DATA__", resp))?;
            Ok(CatalogPage {
                items: FlameComicsAdapter::series_rows(&props),
                has_next: false,
            })
        })
        .await
    }

    async fn list_latest(&self, ctx: &Ctx) -> Result<Vec<LatestUpdate>, AdapterError> {
        // `/browse`, not `/`: the home page's `pageProps` carries a "latest chapters" list keyed
        // by chapter, with no series array, so reading it yielded an empty feed — a valid
        // answer that reports no error while every fast scan does nothing.
        let resp = ctx.fetch("/browse").await?;
        parse_blocking(resp, move |root, _| {
            let props = page_props(root).unwrap_or(Value::Null);
            Ok(FlameComicsAdapter::series_rows(&props)
                .into_iter()
                .map(|item| LatestUpdate {
                    path: item.path,
                    title: item.title,
                    latest_chapter: 0.0,
                })
                .collect())
        })
        .await
    }

    async fn fetch_series(&self, ctx: &Ctx, path: &str) -> Result<SeriesMeta, AdapterError> {
        let resp = ctx.fetch(path).await?;
        parse_blocking(resp, move |root, resp| {
            let props = page_props(root)
                .ok_or_else(|| AdapterError::missing("flamecomics __NEXT_DATA__", resp))?;
            let series = props
                .get("series")
                .cloned()
                .unwrap_or_else(|| props.clone());

            let title = first_str(&series, &["title"])
                .ok_or_else(|| AdapterError::missing("flamecomics series title", resp))?;

            Ok(SeriesMeta {
                alt_titles: series
                    .get("altTitles")
                    .and_then(Value::as_str)
                    .map(crate::html::split_titles)
                    .unwrap_or_default(),
                description: first_str(&series, &["description", "synopsis"]),
                cover_url: first_str(&series, &["cover", "thumbnail"]),
                tags: series
                    .get("tags")
                    .and_then(Value::as_array)
                    .map(|rows| {
                        rows.iter()
                            .filter_map(|t| {
                                t.as_str()
                                    .map(str::to_owned)
                                    .or_else(|| first_str(t, &["name"]))
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                authors: ["author", "artist"]
                    .iter()
                    .filter_map(|k| first_str(&series, &[k]))
                    .fold(Vec::new(), |mut acc: Vec<String>, name| {
                        if !acc.iter().any(|a| a.eq_ignore_ascii_case(&name)) {
                            acc.push(name);
                        }
                        acc
                    }),
                status: first_str(&series, &["status"])
                    .as_deref()
                    .map_or(SeriesStatus::Unknown, crate::html::map_status),
                content_type: content_type_of(&series),
                release_year: None,
                title,
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
        let series_id = Self::id_of(path).to_owned();
        parse_blocking(resp, move |root, resp| {
            let props = page_props(root)
                .ok_or_else(|| AdapterError::missing("flamecomics __NEXT_DATA__", resp))?;
            let rows = props
                .get("chapters")
                .and_then(Value::as_array)
                .ok_or_else(|| AdapterError::missing("flamecomics chapters array", resp))?;

            let mut chapters = Vec::with_capacity(rows.len());
            for row in rows {
                // The site publishes the number as a string (`"168.00"`).
                let Some(number) = row
                    .get("chapter")
                    .and_then(|c| {
                        c.as_f64()
                            .or_else(|| c.as_str().and_then(|s| s.parse().ok()))
                    })
                    .filter(|n: &f64| n.is_finite())
                else {
                    continue;
                };
                // The reader URL is keyed by an opaque per-chapter token, not by its number.
                let Some(token) = first_str(row, &["token"]) else {
                    continue;
                };
                chapters.push(ChapterMeta {
                    number,
                    title: first_str(row, &["title"]),
                    path: format!("/series/{series_id}/{token}"),
                    published_at: epoch_seconds(row, "release_date"),
                    // A chapter still inside its early-access window is not listed here at all
                    // — the payload only carries released ones — so every row that reaches this
                    // point is readable. Marking them locked would hide the whole catalogue.
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
    use super::{FlameComicsAdapter, epoch_seconds};
    use serde_json::json;

    #[test]
    fn series_id_survives_a_trailing_slash() {
        assert_eq!(FlameComicsAdapter::id_of("/series/83"), "83");
        assert_eq!(FlameComicsAdapter::id_of("/series/83/"), "83");
    }

    #[test]
    fn release_dates_are_unix_seconds() {
        let row = json!({"release_date": 1_786_371_624_i64});
        assert_eq!(
            epoch_seconds(&row, "release_date").map(time::OffsetDateTime::year),
            Some(2026)
        );
        assert_eq!(epoch_seconds(&json!({}), "release_date"), None);
    }
}
