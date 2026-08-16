//! Custom adapter for `mgread.io` — the Init Manga `WordPress` theme.
//!
//! Catalogue, latest feed and series metadata are ordinary markup and are delegated to
//! [`GenericConfigAdapter`]. Chapters are not: the series page server-renders only the newest
//! **24** rows and the rest arrive from a REST endpoint keyed by the numeric post id, so a
//! selector-only adapter truncates every long series to its most recent page — silently, with a
//! chapter count that looks plausible and is wrong.

use crate::config::AdapterConfig;
use crate::error::AdapterError;
use crate::generic::GenericConfigAdapter;
use crate::html::{extract_first, parse_blocking, parse_number};
use crate::json::parse_json_body;
use crate::types::{
    CatalogPage, ChapterAccess, ChapterMeta, Ctx, LatestUpdate, SeriesMeta, SourceAdapter,
};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, PrimitiveDateTime, UtcOffset};

/// Chapters requested per API page. `per_page` is capped server-side at this value — asking for
/// more returns 50 and a `total_pages` computed for 50, so overshooting reads as a shorter
/// series rather than failing.
const CHAPTERS_PER_PAGE: u32 = 50;

/// Hard safety cap on API pages walked per series, in case `total_pages` ever stops shrinking
/// the walk. At [`CHAPTERS_PER_PAGE`] this is 10 000 chapters, well past the longest series the
/// site carries.
const MAX_CHAPTER_PAGES: u32 = 200;

/// The chapter endpoint, keyed by the numeric post id.
const CHAPTERS_API: &str = "/wp-json/initmanga/v1/chapters";

/// The `WordPress` core route that resolves a series slug to that id; see
/// [`MgreadAdapter::series_key`].
const POSTS_API: &str = "/wp-json/wp/v2/manga";

/// Everything [`ApiPost`] needs. Asked for explicitly because the unfiltered record embeds the
/// whole rendered series page, which is the cost this route exists to avoid.
const POST_FIELDS: &str = "id,date,date_gmt";

/// Fallback selector for that id, on the series page. The theme puts it on the title element and
/// nowhere else that is unambiguous — the page carries `data-id` on the follow button and on
/// every chapter row too.
const MANGA_ID: &str = "h1#manga-title@data-id";

/// Fallback selector for the site's UTC offset: a chapter row's own timestamp is the only place
/// the markup states one. See [`MgreadAdapter::series_key`].
const ROW_TIMESTAMP: &str = "div.chapter-item time@datetime";

/// The one `lock_type` the endpoint uses for a chapter anybody can read.
const UNLOCKED: &str = "none";

/// The site's own XHR headers. The endpoint is a REST route on the same host as the pages, so
/// a request shaped like a document fetch is the odd one out.
const API_HEADERS: [(&str, &str); 2] = [
    ("Accept", "application/json, text/plain, */*"),
    ("X-Requested-With", "XMLHttpRequest"),
];

/// Selectors for `mgread.io`, merged under the provider's own `config` overrides.
#[must_use]
pub fn mgread_default_config() -> Value {
    json!({
        "catalog": {
            // `/manga/page/1/` redirects to `/manga/`, so one template covers every page.
            "path": "/manga/page/{page}/",
            "item": "div.manga-item-grid",
            "link": "h2 a",
            "title": "h2 a",
            // The theme renders this on every page but the last, which is what ends the walk:
            // the yielded-items fallback would need a page past the end, and that one 404s.
            "next": "link[rel=next]"
        },
        "latest": {
            // A real feed, not the catalogue re-sorted: 24 cards on a route the theme keeps
            // separate from `/manga/`.
            "path": "/recently-updated/",
            "item": "div.manga-item-grid",
            "link": "h2 a",
            "title": "h2 a",
            // The card lists its two newest chapters as buttons; the first is the newest.
            "chapter": "p a"
        },
        "series": {
            "title": "h1#manga-title",
            "desc": "div#manga-description",
            "cover": "a.story-cover img@src",
            // Anchored on the genre archive: the last pill in this container is the theme's
            // estimated reading time (`4h 52m to finish`), an `href="#"` button wearing the same
            // classes as the genres. Interned as a tag it becomes a facet in Discover and a
            // feature in the recommender, one per distinct duration.
            "tags": "div#genre-tags a[href*=\"/genre/\"]",
            "status": "span#manga-status",
            // A labelled row rather than the `span` alone, for the splitting that brings: this
            // cell lists every alternative title in one text node, `;`-separated on most series
            // and `/`-separated on the rest. `split_titles` splits the first and leaves the
            // second whole — the conservative half of that is deliberate, since a title cut at
            // the wrong separator becomes a matching key that pulls unrelated series together.
            "alt": {
                "row": "div.manga-info-details",
                "match": "Alternate Title",
                "value": "span#comic-othername"
            }
        },
        // Present so the config parses, and it is what the site would be read from if the
        // endpoint below ever disappears — but it sees only the newest 24 rows, so
        // `MgreadAdapter` overrides `fetch_chapters` and never uses it.
        "chapters": {
            "container": "div.chapter-list div.chapter-item",
            "link": "a",
            "date": "time@datetime"
        }
    })
}

/// One chapter as the REST endpoint returns it.
#[derive(Debug, Deserialize)]
struct ApiChapter {
    slug: String,
    /// The theme's own parse of the chapter label; `null` on a row it could not read.
    number: Option<f64>,
    /// Empty on every row in the live catalogue — the theme titles a chapter by its number.
    title: Option<String>,
    /// `YYYY-MM-DD HH:MM:SS` in the site's local time, with no offset of its own.
    created_at: Option<String>,
    /// `"none"` for a chapter anybody can read; anything else is a lock.
    lock_type: Option<String>,
}

/// The series post, as the `WordPress` core REST route returns it.
#[derive(Debug, Deserialize)]
struct ApiPost {
    id: u64,
    /// Publication time in the site's local zone…
    date: String,
    /// …and the same instant in UTC. The difference is the site's offset.
    date_gmt: String,
}

impl ApiPost {
    /// The site's UTC offset, as the gap between this post's two timestamps.
    ///
    /// `WordPress` computes `date_gmt` when the post is saved, so this is the offset in effect
    /// then — which is exactly the one the chapter rows of the same series were stored under.
    /// An unreadable or implausible pair reads as UTC rather than shifting dates by a guess.
    fn offset(&self) -> UtcOffset {
        let format =
            time::macros::format_description!("[year]-[month]-[day]T[hour]:[minute]:[second]");
        let (Ok(local), Ok(utc)) = (
            PrimitiveDateTime::parse(self.date.trim(), &format),
            PrimitiveDateTime::parse(self.date_gmt.trim(), &format),
        ) else {
            return UtcOffset::UTC;
        };
        let seconds = (local - utc).whole_seconds();
        i32::try_from(seconds)
            .ok()
            .and_then(|seconds| UtcOffset::from_whole_seconds(seconds).ok())
            .unwrap_or(UtcOffset::UTC)
    }
}

/// The envelope around a page of them.
#[derive(Debug, Deserialize)]
struct ChaptersPage {
    items: Vec<ApiChapter>,
    /// Total pages at the requested `per_page`. Absent would leave the walk with only an empty
    /// page to stop on, which is why it is treated as "this was the last one".
    total_pages: Option<u32>,
}

/// `mgread.io`: themed markup for catalogue/latest/series, REST for the chapter list.
pub struct MgreadAdapter {
    inner: GenericConfigAdapter,
}

impl MgreadAdapter {
    /// Build from an effective (defaults + provider overrides) config.
    #[must_use]
    pub fn new(config: AdapterConfig) -> Self {
        Self {
            inner: GenericConfigAdapter::new(config),
        }
    }

    /// The chapter endpoint's key and the site's UTC offset, for the series at `path`.
    ///
    /// Read from the `WordPress` core REST route, which answers both in about a hundred bytes:
    /// the post `id`, and a `date`/`date_gmt` pair whose difference *is* the offset. The series
    /// page states both too, but it is 130 KB — fetching it once per series here, on top of the
    /// copy `fetch_series` has just read, would double the cost of a full scan of ~6 400 series
    /// for two facts that fit on a line.
    ///
    /// Falls back to the page when the route answers with anything else: WP core routes are
    /// commonly locked down, and this provider is worth keeping on a slower path rather than
    /// losing to a security plugin. A failure that is really the site refusing us resurfaces on
    /// the page fetch, reported properly.
    async fn series_key(ctx: &Ctx, path: &str) -> Result<(String, UtcOffset), AdapterError> {
        let slug = path.rsplit('/').find(|s| !s.is_empty()).unwrap_or(path);
        match ctx
            .fetch_with(
                &format!("{POSTS_API}?slug={slug}&_fields={POST_FIELDS}"),
                &API_HEADERS,
            )
            .await
            .and_then(|resp| parse_json_body::<Vec<ApiPost>>("mgread posts API", &resp))
        {
            Ok(posts) => {
                if let Some(post) = posts.into_iter().next() {
                    return Ok((post.id.to_string(), post.offset()));
                }
                tracing::debug!(
                    provider = %ctx.provider_slug,
                    series = %path,
                    "mgread posts API knows no series by that slug; reading the page instead"
                );
            }
            Err(error) => tracing::debug!(
                provider = %ctx.provider_slug,
                series = %path,
                %error,
                "mgread posts API unavailable; reading the page instead"
            ),
        }

        let resp = ctx.fetch(path).await?;
        parse_blocking(resp, |root, _| {
            let manga_id = extract_first(root, MANGA_ID)?.ok_or_else(|| {
                AdapterError::Missing(format!("manga id (selector {MANGA_ID:?})"))
            })?;
            let offset = extract_first(root, ROW_TIMESTAMP)?
                .and_then(|stamp| OffsetDateTime::parse(&stamp, &Rfc3339).ok())
                .map_or(UtcOffset::UTC, OffsetDateTime::offset);
            Ok((manga_id, offset))
        })
        .await
    }

    /// Resolve one `created_at` against the site's offset.
    ///
    /// `WordPress` stores and the chapter endpoint serves *site-local* time with no offset on it.
    /// Applying the site's own offset is what keeps releases where the site puts them: reading
    /// them as UTC would move every one of them up to a day on a site published from UTC+7, and
    /// hard-coding +07:00 would be wrong the day the operator moves it.
    fn published_at(created_at: &str, offset: UtcOffset) -> Option<OffsetDateTime> {
        let format =
            time::macros::format_description!("[year]-[month]-[day] [hour]:[minute]:[second]");
        PrimitiveDateTime::parse(created_at.trim(), &format)
            .ok()
            .map(|naive| naive.assume_offset(offset))
    }

    /// Fetch one page of the chapter endpoint.
    async fn chapters_page(
        ctx: &Ctx,
        manga_id: &str,
        page: u32,
    ) -> Result<ChaptersPage, AdapterError> {
        let path =
            format!("{CHAPTERS_API}?manga_id={manga_id}&per_page={CHAPTERS_PER_PAGE}&paged={page}");
        let resp = ctx.fetch_with(&path, &API_HEADERS).await?;
        parse_json_body("mgread chapters API", &resp)
    }

    /// The number a `chapter-46-1` slug spells: the theme writes a decimal point as a hyphen.
    ///
    /// Only slugs on that shape are read. A named one (`prologue`, `extra-2`) has no number to
    /// recover, and anything that finds one in it is inventing an ordering.
    fn number_from_slug(slug: &str) -> Option<f64> {
        slug.strip_prefix("chapter-")
            .and_then(|tail| parse_number(&tail.replace('-', ".")))
    }

    /// Turn one API row into a chapter under `series_path`.
    fn chapter(raw: ApiChapter, series_path: &str, offset: UtcOffset) -> Option<ChapterMeta> {
        // The endpoint's own parse is authoritative; the slug is the fallback for a row it left
        // null.
        let number = raw
            .number
            .filter(|n| n.is_finite())
            .or_else(|| Self::number_from_slug(&raw.slug))?;
        Some(ChapterMeta {
            number,
            title: raw.title.filter(|t| !t.trim().is_empty()),
            path: format!("{}/{}/", series_path.trim_end_matches('/'), raw.slug),
            published_at: raw
                .created_at
                .as_deref()
                .and_then(|stamp| Self::published_at(stamp, offset)),
            // No live row carries anything but `"none"`, so what a lock is *called* here is
            // unverified: any other value is read as locked, because selling a paid chapter to a
            // reader as free is the failure that costs them something.
            access: match raw.lock_type.as_deref() {
                Some(UNLOCKED) | None => ChapterAccess::Free,
                Some(_) => ChapterAccess::EarlyAccess { unlocks_at: None },
            },
        })
    }
}

#[async_trait]
impl SourceAdapter for MgreadAdapter {
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
        let (manga_id, offset) = Self::series_key(ctx, path).await?;

        let series_path = path.to_owned();
        let mut chapters = Vec::new();
        let mut unnumbered = 0usize;
        for page in 1..=MAX_CHAPTER_PAGES {
            let body = Self::chapters_page(ctx, &manga_id, page).await?;
            let total_pages = body.total_pages.unwrap_or(page);
            let received = body.items.len();
            chapters.reserve(received);
            for raw in body.items {
                match Self::chapter(raw, &series_path, offset) {
                    Some(chapter) => chapters.push(chapter),
                    None => unnumbered += 1,
                }
            }
            // An empty page ends the walk as well as an exhausted count: `total_pages` is the
            // endpoint's arithmetic, and it recomputes it against a `per_page` it may have
            // clamped.
            if page >= total_pages || received == 0 {
                break;
            }
        }
        if unnumbered > 0 {
            tracing::warn!(
                provider = %ctx.provider_slug,
                series = %path,
                unnumbered,
                "mgread chapters API returned rows with no usable number"
            );
        }
        Ok(chapters)
    }
}

#[cfg(test)]
mod tests {
    use super::{ApiChapter, MgreadAdapter};
    use crate::types::ChapterAccess;
    use time::UtcOffset;
    use time::macros::datetime;

    fn row(slug: &str, number: Option<f64>, lock: Option<&str>) -> ApiChapter {
        ApiChapter {
            slug: slug.to_owned(),
            number,
            title: None,
            created_at: Some("2026-08-15 14:44:29".to_owned()),
            lock_type: lock.map(str::to_owned),
        }
    }

    #[test]
    fn a_chapter_path_is_the_slug_under_the_series() {
        let chapter = MgreadAdapter::chapter(
            row("chapter-367", Some(367.0), Some("none")),
            "/manga/global-martial-arts/",
            UtcOffset::UTC,
        )
        .expect("numbered row");
        assert_eq!(
            chapter.path, "/manga/global-martial-arts/chapter-367/",
            "the reader link is the series path plus the chapter slug"
        );
        assert_eq!(chapter.access, ChapterAccess::Free);
    }

    #[test]
    fn a_row_the_endpoint_could_not_number_falls_back_to_its_slug() {
        let chapter =
            MgreadAdapter::chapter(row("chapter-46-1", None, None), "/manga/x", UtcOffset::UTC)
                .expect("slug carries the number");
        assert!((chapter.number - 46.1).abs() < f64::EPSILON);
    }

    #[test]
    fn an_unnumbered_row_is_dropped_rather_than_guessed() {
        assert!(
            MgreadAdapter::chapter(row("prologue", None, None), "/manga/x", UtcOffset::UTC)
                .is_none()
        );
    }

    #[test]
    fn any_lock_type_but_none_keeps_the_chapter_locked() {
        let chapter = MgreadAdapter::chapter(
            row("chapter-1", Some(1.0), Some("coin")),
            "/x",
            UtcOffset::UTC,
        )
        .expect("numbered row");
        assert_eq!(
            chapter.access,
            ChapterAccess::EarlyAccess { unlocks_at: None }
        );
    }

    /// Bug guard: the endpoint serves `WordPress`'s stored *site-local* time with no offset on it,
    /// while the rows on the page it came from carry `+07:00`. Read as UTC, every release on
    /// this site lands seven hours late — enough to move a chapter into the following day in the
    /// release feed.
    #[test]
    fn a_created_at_resolves_against_the_offset_the_page_reported() {
        let offset = UtcOffset::from_hms(7, 0, 0).expect("valid offset");
        assert_eq!(
            MgreadAdapter::published_at("2026-08-15 14:44:29", offset),
            Some(datetime!(2026-08-15 14:44:29 +7))
        );
        assert_eq!(
            MgreadAdapter::published_at("not a timestamp", offset),
            None,
            "an unparseable stamp leaves the date unset rather than guessed"
        );
    }
}
