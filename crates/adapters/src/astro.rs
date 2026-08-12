//! Sites built on Astro islands (here: Asura Scans and `HiveToons`).
//!
//! Both render their catalogue, series metadata and chapter list from React islands, and their
//! DOM carries nothing but Tailwind utility classes — there is no `.chapter-row` to select, only
//! `class="group flex items-center justify-between px-4 py-4"`. A selector set written against
//! that breaks on any restyle.
//!
//! The island's `props` attribute is the better contract: it is the component's own input,
//! named by the component's own field names, and it carries strictly more than the DOM does —
//! including the paywall flags (`is_locked`/`unlock_time` on Asura, `isLocked`/`isTimeLocked` on
//! `HiveToons`) that the rendered page only expresses as an icon.

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
use time::format_description::well_known::Rfc3339;

/// Which site's field names to read. The transport, the island decoding and the paging are
/// identical; only the names differ, so this is an enum rather than two adapters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AstroFlavour {
    /// `asurascans.com`.
    Asura,
    /// `hivetoons.org`.
    HiveToons,
}

impl AstroFlavour {
    /// The slug the factory dispatches on, or `None` for a slug this adapter does not serve.
    #[must_use]
    pub fn from_slug(slug: &str) -> Option<Self> {
        match slug {
            "asura" => Some(Self::Asura),
            "hivetoons" => Some(Self::HiveToons),
            _ => None,
        }
    }

    /// Catalogue path template, `{page}` substituted.
    fn catalog_path(self) -> &'static str {
        match self {
            Self::Asura => "/browse?page={page}",
            Self::HiveToons => "/series?page={page}",
        }
    }

    /// The island prop holding the catalogue page's series array, and the one holding the
    /// collection total.
    fn catalog_keys(self) -> (&'static str, &'static str) {
        match self {
            Self::Asura => ("initialSeries", "totalCount"),
            Self::HiveToons => ("initialPosts", "initialTotalCount"),
        }
    }

    /// The prop holding the chapter array on a series page.
    fn chapters_key(self) -> &'static str {
        match self {
            Self::Asura => "chapters",
            Self::HiveToons => "initialChap",
        }
    }

    /// Series-path prefix, which is also how a catalogue slug becomes a stored path.
    fn series_prefix(self) -> &'static str {
        match self {
            Self::Asura => "/comics",
            Self::HiveToons => "/series",
        }
    }
}

/// Astro serialises every prop value as a `[type_tag, value]` pair; this unwraps them.
///
/// A genuine two-element array whose first element is a number would be misread as a wrapper.
/// No prop in either site's payload has that shape, and the alternative — threading Astro's
/// type tags through — buys nothing, since every tag in use here denotes a plain JSON value.
fn unwrap_astro(value: Value) -> Value {
    match value {
        Value::Array(items) if items.len() == 2 && items[0].is_number() => {
            unwrap_astro(items.into_iter().nth(1).unwrap_or(Value::Null))
        }
        Value::Array(items) => Value::Array(items.into_iter().map(unwrap_astro).collect()),
        Value::Object(map) => {
            Value::Object(map.into_iter().map(|(k, v)| (k, unwrap_astro(v))).collect())
        }
        other => other,
    }
}

/// The first island whose decoded props contain `key`, or `None`.
///
/// A page carries a dozen islands and only one of them is the list being looked for, so this
/// selects by the presence of the field rather than by island order or component name — both of
/// which change when the page is re-laid-out and neither of which is part of any contract.
fn island_with(root: ElementRef<'_>, key: &str) -> Option<Value> {
    let selector = crate::html::parse_selector("astro-island").ok()?;
    for island in root.select(&selector) {
        // `scraper` has already resolved the HTML entities the attribute is encoded with.
        let Some(raw) = island.value().attr("props") else {
            continue;
        };
        let Ok(parsed) = serde_json::from_str::<Value>(raw) else {
            continue;
        };
        let decoded = unwrap_astro(parsed);
        if decoded.get(key).is_some() {
            return Some(decoded);
        }
    }
    None
}

/// Read the first present string field out of `candidates`.
fn first_str(value: &Value, candidates: &[&str]) -> Option<String> {
    candidates
        .iter()
        .find_map(|k| value.get(*k).and_then(Value::as_str))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Read a chapter number from whichever numeric or string field the site uses.
fn chapter_number(value: &Value) -> Option<f64> {
    let raw = value.get("number").or_else(|| value.get("chapter"))?;
    raw.as_f64()
        .or_else(|| raw.as_str().and_then(|s| s.parse().ok()))
        .filter(|n: &f64| n.is_finite())
}

/// The access state a chapter object advertises.
///
/// Asura publishes both a flag and a date (`is_locked` + `unlock_time`/`early_access_until`).
/// `HiveToons` publishes flags only (`isLocked`, and `isPermanentlyLocked` for chapters that
/// never open), so its locked chapters carry no unlock time — which the read paths treat as
/// still locked, the conservative and correct reading.
fn access_of(value: &Value) -> ChapterAccess {
    let locked = ["is_locked", "isLocked"]
        .iter()
        .any(|k| value.get(*k).and_then(Value::as_bool) == Some(true));
    if !locked {
        return ChapterAccess::Free;
    }
    // A permanent lock is not early access with an unknown date, but it is stored the same way:
    // both are "not readable, and no date says otherwise".
    let unlocks_at = first_str(value, &["unlock_time", "early_access_until", "freeAt"])
        .and_then(|s| OffsetDateTime::parse(&s, &Rfc3339).ok());
    ChapterAccess::EarlyAccess { unlocks_at }
}

/// Map either site's status vocabulary onto the domain's.
fn status_of(value: &Value) -> SeriesStatus {
    first_str(value, &["status", "postStatus", "seriesStatus"])
        .as_deref()
        .map_or(SeriesStatus::Unknown, crate::html::map_status)
}

/// Whether a catalogue row is a text novel rather than a comic.
///
/// `HiveToons` sells both under one catalogue and one URL prefix, and a novel's "chapter" is
/// prose — there are no pages to read and nothing this application can track. Six of the first
/// 126 rows are novels, and they were being ingested as ordinary series.
///
/// The medium is what decides, not the URL: the platform stores prose at `/series/<slug>` like
/// everything else.
fn is_prose(row: &Value) -> bool {
    first_str(row, &["type", "postType", "seriesType"])
        .is_some_and(|t| t.eq_ignore_ascii_case("novel") || t.eq_ignore_ascii_case("light_novel"))
        || row.get("isNovel").and_then(Value::as_bool) == Some(true)
}

/// Map either site's medium vocabulary onto the domain's.
fn content_type_of(value: &Value) -> ContentType {
    match first_str(value, &["type", "postType", "seriesType"])
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("manga") => ContentType::Manga,
        Some("manhwa") => ContentType::Manhwa,
        Some("manhua") => ContentType::Manhua,
        Some("webtoon") => ContentType::Webtoon,
        _ => ContentType::Unknown,
    }
}

/// An Astro-island-backed provider.
pub struct AstroIslandAdapter {
    flavour: AstroFlavour,
}

impl AstroIslandAdapter {
    /// Build for a dispatch slug.
    ///
    /// # Errors
    /// [`AdapterError::UnknownCustom`] for a slug this adapter does not serve.
    pub fn new(slug: &str) -> Result<Self, AdapterError> {
        AstroFlavour::from_slug(slug)
            .map(|flavour| Self { flavour })
            .ok_or_else(|| AdapterError::UnknownCustom(slug.to_owned()))
    }

    /// Series entries out of a catalogue island, and how many rows the island actually held.
    ///
    /// The two numbers differ because prose is dropped here (see [`is_prose`]), and the caller
    /// needs the *unfiltered* count: `has_next` is decided against the site's own collection
    /// total, so paging on the filtered length would walk past the end of a catalogue whose
    /// pages contain novels.
    fn catalog_items(flavour: AstroFlavour, island: &Value) -> (Vec<CatalogItem>, usize) {
        let (list_key, _) = flavour.catalog_keys();
        let Some(rows) = island.get(list_key).and_then(Value::as_array) else {
            return (Vec::new(), 0);
        };
        let items = rows
            .iter()
            .filter(|row| !is_prose(row))
            .filter_map(|row| {
                let slug = first_str(row, &["slug", "series_slug", "seriesSlug"])?;
                Some(CatalogItem {
                    path: format!("{}/{slug}", flavour.series_prefix()),
                    title: first_str(row, &["title", "postTitle", "name"])
                        .unwrap_or_else(|| slug.clone()),
                })
            })
            .collect();
        (items, rows.len())
    }
}

#[async_trait]
impl SourceAdapter for AstroIslandAdapter {
    async fn list_catalog(&self, ctx: &Ctx, page: u32) -> Result<CatalogPage, AdapterError> {
        let path = self
            .flavour
            .catalog_path()
            .replace("{page}", &page.to_string());
        let resp = ctx.fetch(&path).await?;
        let flavour = self.flavour;
        parse_blocking(resp, move |root, resp| {
            let (list_key, total_key) = flavour.catalog_keys();
            let island = island_with(root, list_key).ok_or_else(|| {
                AdapterError::missing(&format!("astro island carrying `{list_key}`"), resp)
            })?;
            let (items, rows) = AstroIslandAdapter::catalog_items(flavour, &island);
            // The island states the collection total, so the walk ends on the site's own count
            // rather than on "this page had rows" — which never goes false on either site,
            // because both re-serve the last page for any page number past the end.
            //
            // `rows`, not `items.len()`: the total counts novels, so a page whose prose rows
            // were dropped still consumed a full page of it. Paging on the filtered length
            // under-counts and walks past the end.
            let total = island.get(total_key).and_then(Value::as_u64).unwrap_or(0);
            let seen = u64::from(page) * rows as u64;
            let has_next = rows > 0 && seen < total;
            Ok(CatalogPage { items, has_next })
        })
        .await
    }

    async fn list_latest(&self, ctx: &Ctx) -> Result<Vec<LatestUpdate>, AdapterError> {
        // The catalogue's first page, not the home page. The home page renders its own islands
        // (carousels, a hero, a "latest chapters" strip) and none of them is the series list —
        // so reading it returned an empty feed, and an empty feed is a valid answer that no
        // error path reports. Every fast scan silently did nothing.
        let path = self.flavour.catalog_path().replace("{page}", "1");
        let resp = ctx.fetch(&path).await?;
        let flavour = self.flavour;
        parse_blocking(resp, move |root, _| {
            let (list_key, _) = flavour.catalog_keys();
            Ok(island_with(root, list_key)
                .map(|island| AstroIslandAdapter::catalog_items(flavour, &island).0)
                .unwrap_or_default()
                .into_iter()
                .map(|item| LatestUpdate {
                    path: item.path,
                    title: item.title,
                    // Neither island publishes a per-series latest chapter number that agrees
                    // with the chapter list; both fast-scan callers re-ingest by path anyway.
                    latest_chapter: 0.0,
                })
                .collect())
        })
        .await
    }

    async fn fetch_series(&self, ctx: &Ctx, path: &str) -> Result<SeriesMeta, AdapterError> {
        let resp = ctx.fetch(path).await?;
        let flavour = self.flavour;
        parse_blocking(resp, move |root, resp| {
            // Asura publishes the metadata island flat; HiveToons nests it under `post`.
            let island = match flavour {
                AstroFlavour::Asura => {
                    island_with(root, "alternativeTitles").or_else(|| island_with(root, "coverUrl"))
                }
                AstroFlavour::HiveToons => {
                    island_with(root, "post").and_then(|v| v.get("post").cloned())
                }
            }
            .ok_or_else(|| AdapterError::missing("astro series metadata island", resp))?;

            let title = first_str(&island, &["title", "postTitle"])
                .ok_or_else(|| AdapterError::missing("astro series title", resp))?;

            let alt_titles = island
                .get("alternativeTitles")
                .and_then(Value::as_str)
                .map(crate::html::split_titles)
                .unwrap_or_default();

            let tags = island
                .get("genres")
                .and_then(Value::as_array)
                .map(|rows| {
                    rows.iter()
                        .filter_map(|g| {
                            g.as_str()
                                .map(str::to_owned)
                                .or_else(|| first_str(g, &["name"]))
                        })
                        .collect()
                })
                .unwrap_or_default();

            let authors = ["author", "artist"]
                .iter()
                .filter_map(|k| first_str(&island, &[k]))
                .fold(Vec::new(), |mut acc: Vec<String>, name| {
                    if !acc.iter().any(|a| a.eq_ignore_ascii_case(&name)) {
                        acc.push(name);
                    }
                    acc
                });

            Ok(SeriesMeta {
                title,
                alt_titles,
                // `HiveToons` stores the synopsis as the HTML the editor typed, so it reaches
                // the reader as literal `<p>` tags unless it is flattened here. Asura's is
                // already plain text, and text through a fragment parse is itself.
                description: first_str(&island, &["description", "postContent"])
                    .map(|d| crate::html::text_from_fragment(&d))
                    .filter(|d| !d.is_empty()),
                cover_url: first_str(&island, &["coverUrl", "featuredImage"]),
                tags,
                authors,
                status: status_of(&island),
                content_type: content_type_of(&island),
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
        let flavour = self.flavour;
        let series_path = path.trim_end_matches('/').to_owned();
        parse_blocking(resp, move |root, resp| {
            let key = flavour.chapters_key();
            let island = island_with(root, key)
                .ok_or_else(|| AdapterError::missing(&format!("astro island `{key}`"), resp))?;
            let rows = island
                .get(key)
                .and_then(Value::as_array)
                .ok_or_else(|| AdapterError::missing(&format!("astro `{key}` array"), resp))?;

            let mut chapters = Vec::with_capacity(rows.len());
            for row in rows {
                let Some(number) = chapter_number(row) else {
                    continue;
                };
                // Built from the *stored* series path, not from the island's `series_slug`:
                // Asura's reader URL carries a hash suffix (`…-7e1f454a`) that the island's
                // slug does not, so a path assembled from the island 404s.
                let slug = first_str(row, &["slug"]);
                let chapter_path = match (flavour, slug) {
                    (AstroFlavour::Asura, _) => {
                        format!("{series_path}/chapter/{}", trim_number(number))
                    }
                    (AstroFlavour::HiveToons, Some(slug)) => format!("{series_path}/{slug}"),
                    (AstroFlavour::HiveToons, None) => {
                        format!("{series_path}/chapter-{}", trim_number(number))
                    }
                };
                chapters.push(ChapterMeta {
                    number,
                    title: first_str(row, &["title"]),
                    path: chapter_path,
                    published_at: first_str(row, &["published_at", "createdAt", "created_at"])
                        .and_then(|s| OffsetDateTime::parse(&s, &Rfc3339).ok()),
                    access: access_of(row),
                });
            }
            Ok(chapters)
        })
        .await
    }
}

/// Render a chapter number the way the sites spell it in a URL: `196`, not `196`.
fn trim_number(number: f64) -> String {
    if number.fract() == 0.0 {
        format!("{number:.0}")
    } else {
        let s = format!("{number}");
        s.trim_end_matches('0').trim_end_matches('.').to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AstroFlavour, AstroIslandAdapter, access_of, chapter_number, is_prose, trim_number,
        unwrap_astro,
    };
    use crate::types::ChapterAccess;
    use serde_json::json;
    use time::macros::datetime;

    /// Astro wraps every value as `[tag, value]`, including nested ones. Decoding only the top
    /// level leaves every field as a two-element array, which reads as "no such field".
    #[test]
    fn astro_tuples_unwrap_at_every_depth() {
        let raw = json!({
            "chapters": [0, [[0, {"number": [0, 196], "is_locked": [0, true]}]]]
        });
        let decoded = unwrap_astro(raw);
        assert_eq!(decoded["chapters"][0]["number"], json!(196));
        assert_eq!(decoded["chapters"][0]["is_locked"], json!(true));
    }

    #[test]
    fn a_free_chapter_is_free_on_both_sites() {
        assert_eq!(access_of(&json!({"is_locked": false})), ChapterAccess::Free);
        assert_eq!(access_of(&json!({"isLocked": false})), ChapterAccess::Free);
        // No flag at all is also free: absence of a paywall marker is not a paywall.
        assert_eq!(access_of(&json!({"number": 1})), ChapterAccess::Free);
    }

    #[test]
    fn asura_locked_chapters_carry_their_unlock_time() {
        let row = json!({"is_locked": true, "unlock_time": "2026-08-10T20:21:18Z"});
        assert_eq!(
            access_of(&row),
            ChapterAccess::EarlyAccess {
                unlocks_at: Some(datetime!(2026-08-10 20:21:18 UTC))
            }
        );
    }

    /// `HiveToons` publishes the flag without a date. Reading that as "unlocks now" would put a
    /// chapter the reader cannot open into their unread count.
    #[test]
    fn hivetoons_locked_chapters_have_no_date_and_stay_locked() {
        let row = json!({"isLocked": true, "isTimeLocked": true, "price": 10});
        assert_eq!(
            access_of(&row),
            ChapterAccess::EarlyAccess { unlocks_at: None }
        );
    }

    /// Regression: `HiveToons` sells prose novels from the same catalogue, under the same
    /// `/series/<slug>` prefix, and they were ingested as ordinary comics — a "chapter" of one
    /// is text, so there is nothing to read and nothing to track. The catalogue row says which
    /// it is; nothing was reading it.
    #[test]
    fn a_novel_row_is_prose_and_a_comic_row_is_not() {
        assert!(is_prose(&json!({"seriesType": "NOVEL"})));
        assert!(is_prose(&json!({"seriesType": "novel"})));
        assert!(is_prose(&json!({"isNovel": true})));
        assert!(!is_prose(&json!({"seriesType": "MANHWA"})));
        assert!(!is_prose(&json!({"seriesType": "MANGA"})));
        // No medium stated is not prose: the catalogue is comics by default, and dropping an
        // unlabelled row would silently shrink every provider that omits the field.
        assert!(!is_prose(&json!({"slug": "x"})));
    }

    /// The walk ends on the site's own collection total, so paging has to count the rows the
    /// page *held*, not the ones that survived the filter — otherwise a catalogue containing
    /// novels never reaches its own end and the walk runs to the planner's cap.
    #[test]
    fn dropping_a_novel_does_not_shorten_the_page_the_walk_counts() {
        let island = json!({
            "initialPosts": [
                {"slug": "a", "postTitle": "A", "seriesType": "MANHWA"},
                {"slug": "b", "postTitle": "B", "seriesType": "NOVEL"},
                {"slug": "c", "postTitle": "C", "seriesType": "MANHUA"},
            ]
        });
        let (items, rows) = AstroIslandAdapter::catalog_items(AstroFlavour::HiveToons, &island);
        assert_eq!(rows, 3, "the page held three rows");
        assert_eq!(items.len(), 2, "one of them was prose");
        assert!(items.iter().all(|i| i.path.starts_with("/series/")));
        assert!(!items.iter().any(|i| i.path.ends_with("/b")));
    }

    #[test]
    fn chapter_numbers_read_from_either_encoding() {
        assert_eq!(chapter_number(&json!({"number": 196})), Some(196.0));
        assert_eq!(chapter_number(&json!({"number": "196.5"})), Some(196.5));
        assert_eq!(chapter_number(&json!({"title": "x"})), None);
    }

    #[test]
    fn chapter_numbers_render_without_a_trailing_zero() {
        assert_eq!(trim_number(196.0), "196");
        assert_eq!(trim_number(196.5), "196.5");
    }

    #[test]
    fn only_the_two_registered_slugs_build() {
        assert_eq!(AstroFlavour::from_slug("asura"), Some(AstroFlavour::Asura));
        assert_eq!(
            AstroFlavour::from_slug("hivetoons"),
            Some(AstroFlavour::HiveToons)
        );
        assert_eq!(AstroFlavour::from_slug("something-else"), None);
    }
}
