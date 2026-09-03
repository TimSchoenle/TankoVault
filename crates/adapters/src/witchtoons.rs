//! `WitchToons` — the Next.js App Router platform `witchscans.com` moved to.
//!
//! The DOM is Tailwind utility classes and the series page server-renders exactly one chapter
//! link (the "start reading" button), so a selector set reads a full catalogue and zero chapters —
//! the silent failure an empty parse always is. What the page *does* carry is React's flight
//! payload: the server components' own props, streamed as `self.__next_f.push([1,"…"])` chunks,
//! naming every field the platform models — chapter numbers, publish times, and the paywall state
//! (`isLocked`, `earlyAccessUntil`) that the rendered page only expresses as a badge.
//!
//! The site's JSON API would be simpler still. It is `Disallow: /api/*` in `robots.txt`, so this
//! reads the same data out of the HTML the crawler is allowed to fetch.

use crate::error::AdapterError;
use crate::html::{absolutize, map_status, parse_blocking, text_from_fragment};
use crate::medium::is_prose_medium;
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

/// The chunk call that carries one slice of the flight payload.
const CHUNK_MARKER: &str = "self.__next_f.push([1,";

/// Upper bound on values decoded for one key in one document. Every key this adapter reads is
/// answered by its first or second occurrence; the cap only stops a pathological document from
/// turning a lookup into a full re-parse of the payload per candidate.
const MAX_CANDIDATES: usize = 8;

/// Comics live under this prefix; the platform's prose lives under a sibling one and is listed
/// separately, so `/series` and the home feed yield comics alone. [`is_prose`] enforces that
/// rather than trusting it — a novel reaching here would be stored at a comic URL that 404s.
const SERIES_PREFIX: &str = "/series/comic";

/// The React flight payload of a rendered page, with every streamed chunk concatenated.
///
/// Each chunk's argument is a JSON string literal, so the chunks are decoded rather than
/// concatenated raw: a value routinely straddles a chunk boundary, and the escapes are only
/// balanced once the literals are decoded.
fn flight_payload(root: ElementRef<'_>) -> String {
    let Ok(scripts) = crate::html::parse_selector("script") else {
        return String::new();
    };
    let mut payload = String::new();
    for script in root.select(&scripts) {
        let text = script.text().collect::<String>();
        let mut rest = text.as_str();
        while let Some(at) = rest.find(CHUNK_MARKER) {
            rest = &rest[at + CHUNK_MARKER.len()..];
            let mut chunks = serde_json::Deserializer::from_str(rest).into_iter::<String>();
            match chunks.next() {
                Some(Ok(chunk)) => {
                    payload.push_str(&chunk);
                    rest = &rest[chunks.byte_offset()..];
                }
                _ => break,
            }
        }
    }
    payload
}

/// Every value the payload binds to `key`, in document order, up to [`MAX_CANDIDATES`].
///
/// The payload is a stream of component props rather than one document, so a key is not unique:
/// `series` is the series object on a series page and the update feed on the home page, and both
/// pages carry other bindings of it. Callers pick by shape, which is the only stable contract —
/// component and route names change with any re-layout.
fn values_for_key(payload: &str, key: &str) -> Vec<Value> {
    let needle = format!("\"{key}\":");
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(at) = payload[from..].find(&needle) {
        let value_at = from + at + needle.len();
        from = value_at;
        let mut values = serde_json::Deserializer::from_str(&payload[value_at..]).into_iter();
        if let Some(Ok(value)) = values.next() {
            found.push(value);
            if found.len() == MAX_CANDIDATES {
                break;
            }
        }
    }
    found
}

/// The first value bound to `key` that satisfies `wanted`.
fn value_matching(payload: &str, key: &str, wanted: impl Fn(&Value) -> bool) -> Option<Value> {
    values_for_key(payload, key).into_iter().find(|v| wanted(v))
}

/// A non-empty trimmed string field.
fn field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Whether a listing row is prose rather than a comic.
///
/// The medium decides, not the section it was found in: `HiveToons` sells both under one
/// catalogue and its novels were ingested as series until the same check was added there. A novel
/// here would be stored under [`SERIES_PREFIX`], which is not where the platform serves it.
fn is_prose(row: &Value) -> bool {
    field(row, "type").is_some_and(|t| is_prose_medium(&t))
}

/// The stored path for a catalogue or feed row: `urlSlug` if the platform states one, else `slug`.
fn series_path(row: &Value) -> Option<String> {
    if is_prose(row) {
        return None;
    }
    let slug = field(row, "urlSlug").or_else(|| field(row, "slug"))?;
    Some(format!("{SERIES_PREFIX}/{slug}"))
}

/// A chapter number from a row, which the platform publishes as a JSON number.
fn chapter_number(row: &Value) -> Option<f64> {
    let raw = row.get("number")?;
    raw.as_f64()
        .or_else(|| raw.as_str().and_then(|s| s.parse().ok()))
        .filter(|n: &f64| n.is_finite())
}

/// Render a chapter number the way the reader URL spells it: `/chapter/62`, `/chapter/62.5`.
fn number_in_url(number: f64) -> String {
    if number.fract() == 0.0 {
        format!("{number:.0}")
    } else {
        format!("{number}")
    }
}

/// A chapter title worth storing, which is not the chapter's own number.
///
/// The platform defaults an untitled chapter's `title` to the number as a string, so storing it
/// verbatim gives every second chapter the title "62" — noise that then renders next to the
/// number it duplicates.
fn chapter_title(row: &Value, number: f64) -> Option<String> {
    let title = field(row, "title")?;
    (title != number_in_url(number)).then_some(title)
}

/// The access state a chapter row advertises.
///
/// `becomesFreeOnNextRelease` is deliberately not read as an unlock time: it is a rule, not a
/// date, and a locked chapter with no announced time stays locked — the conservative reading the
/// rest of the pipeline expects.
fn access_of(row: &Value) -> ChapterAccess {
    if row.get("isLocked").and_then(Value::as_bool) != Some(true) {
        return ChapterAccess::Free;
    }
    let unlocks_at = ["earlyAccessUntil", "becomesFreeAt", "unlockedAt"]
        .iter()
        .find_map(|k| field(row, k))
        .and_then(|s| OffsetDateTime::parse(&s, &Rfc3339).ok());
    ChapterAccess::EarlyAccess { unlocks_at }
}

/// Map the platform's medium vocabulary onto the domain's.
fn content_type_of(value: &Value) -> ContentType {
    match field(value, "type")
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

/// A genre name without the emoji the platform decorates it with.
///
/// Every genre — and only genres — is published as `"Action ⚔️"`. Stored verbatim it becomes its
/// own vocabulary entry, so `Action` from any other provider never merges with it, the Discover
/// facet renders the emoji, and a variation-selector byte decides whether two spellings of one
/// genre are the same tag. Trailing non-alphanumerics are the decoration; anything a genuine name
/// ends in survives, and a name that is decoration alone falls back to the slug.
fn strip_decoration(name: &str) -> String {
    name.trim_end_matches(|c: char| !c.is_alphanumeric() && c != ')')
        .trim()
        .to_owned()
}

/// Tag names out of the two shapes the payload uses: `tags` is `{name, slug}`, and `genres` is
/// either the same shape or — on a catalogue row — a slug nested one level deeper as
/// `{genre: {slug}}`.
fn tags_of(value: &Value) -> Vec<String> {
    let mut tags: Vec<String> = Vec::new();
    for key in ["tags", "genres"] {
        let Some(rows) = value.get(key).and_then(Value::as_array) else {
            continue;
        };
        for row in rows {
            let source = row.get("genre").unwrap_or(row);
            let name = field(source, "name")
                .map(|n| strip_decoration(&n))
                .filter(|n| !n.is_empty())
                .or_else(|| field(source, "slug"));
            if let Some(name) = name
                && !tags.iter().any(|t| t.eq_ignore_ascii_case(&name))
            {
                tags.push(name);
            }
        }
    }
    tags
}

/// Whether a value is the series object rather than one of the payload's other `series` bindings.
fn is_series_object(value: &Value) -> bool {
    value.is_object() && value.get("title").is_some() && value.get("slug").is_some()
}

/// The chapter rows a series page carries, or `None` when the page is not one.
///
/// The distinction the shape test alone cannot make: a series the platform lists before its first
/// chapter is published binds `chapters` to `[]`, which looks exactly like a payload that was
/// never found. The series object separates them — present, this is a series page with nothing to
/// ingest yet; absent, the parse genuinely failed and has to stay loud, because silently
/// returning "no chapters" would leave the series empty forever with nothing reporting it.
fn chapter_rows(payload: &str) -> Option<Vec<Value>> {
    let populated = |value: &Value| {
        value
            .as_array()
            .and_then(|rows| rows.first())
            .is_some_and(|row| row.get("number").is_some())
    };
    if let Some(Value::Array(rows)) = value_matching(payload, "chapters", populated) {
        return Some(rows);
    }
    value_matching(payload, "series", is_series_object).map(|_| Vec::new())
}

/// Whether a value is the home page's update feed: an array whose rows state when they last
/// gained a chapter. The same key also binds carousels and a novel strip, neither of which does.
fn is_update_feed(value: &Value) -> bool {
    value
        .as_array()
        .and_then(|rows| rows.first())
        .is_some_and(|row| row.get("lastChapterAt").is_some())
}

/// The `WitchToons` platform adapter.
pub struct WitchToonsAdapter;

impl WitchToonsAdapter {
    /// Build the adapter.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Default for WitchToonsAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SourceAdapter for WitchToonsAdapter {
    async fn list_catalog(&self, ctx: &Ctx, page: u32) -> Result<CatalogPage, AdapterError> {
        let resp = ctx.fetch(&format!("/series?page={page}")).await?;
        parse_blocking(resp, move |root, resp| {
            let payload = flight_payload(root);
            let rows = value_matching(&payload, "initialSeries", Value::is_array)
                .ok_or_else(|| AdapterError::missing("witchtoons `initialSeries`", resp))?;
            let items = rows
                .as_array()
                .map(|rows| {
                    rows.iter()
                        .filter_map(|row| {
                            let path = series_path(row)?;
                            let title = field(row, "title").unwrap_or_else(|| path.clone());
                            Some(CatalogItem { path, title })
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            // The listing states whether another page exists, so the walk ends on the site's own
            // answer rather than on "this page had rows".
            //
            // The listing's order is not stable across requests and the backend has no
            // tiebreaker, so consecutive pages overlap: one walk of the 114-row catalogue
            // enumerates about 103 distinct series. Nothing here can fix that — no sort
            // parameter is honoured — and successive scans converge, which is why the walk is
            // still bounded by the site's own answer rather than repeated until it saturates.
            let has_next = value_matching(&payload, "initialHasMore", Value::is_boolean)
                .and_then(|v| v.as_bool())
                .unwrap_or(!items.is_empty());
            Ok(CatalogPage { items, has_next })
        })
        .await
    }

    async fn list_latest(&self, ctx: &Ctx) -> Result<Vec<LatestUpdate>, AdapterError> {
        let resp = ctx.fetch("/").await?;
        parse_blocking(resp, move |root, resp| {
            let payload = flight_payload(root);
            let rows = value_matching(&payload, "series", is_update_feed)
                .ok_or_else(|| AdapterError::missing("witchtoons update feed", resp))?;
            Ok(rows
                .as_array()
                .map(|rows| {
                    rows.iter()
                        .filter_map(|row| {
                            let path = series_path(row)?;
                            let title = field(row, "title").unwrap_or_else(|| path.clone());
                            let latest_chapter = row
                                .get("chapters")
                                .and_then(Value::as_array)
                                .and_then(|chapters| {
                                    chapters.iter().filter_map(chapter_number).reduce(f64::max)
                                })
                                .unwrap_or(0.0);
                            Some(LatestUpdate {
                                path,
                                title,
                                latest_chapter,
                            })
                        })
                        .collect()
                })
                .unwrap_or_default())
        })
        .await
    }

    async fn fetch_series(&self, ctx: &Ctx, path: &str) -> Result<SeriesMeta, AdapterError> {
        let resp = ctx.fetch(path).await?;
        parse_blocking(resp, move |root, resp| {
            let payload = flight_payload(root);
            let series = value_matching(&payload, "series", is_series_object)
                .ok_or_else(|| AdapterError::missing("witchtoons series object", resp))?;
            let title = field(&series, "title")
                .ok_or_else(|| AdapterError::missing("witchtoons series title", resp))?;

            let mut alt_titles: Vec<String> = ["altTitle", "originalTitle"]
                .iter()
                .filter_map(|k| field(&series, k))
                .collect();
            if let Some(aliases) = series.get("aliases").and_then(Value::as_array) {
                alt_titles.extend(aliases.iter().filter_map(|a| {
                    a.as_str()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                }));
            }

            Ok(SeriesMeta {
                title,
                alt_titles,
                description: field(&series, "description")
                    .map(|d| text_from_fragment(&d))
                    .filter(|d| !d.is_empty()),
                cover_url: field(&series, "coverImage").map(|c| absolutize(&resp.url, &c)),
                tags: tags_of(&series),
                // The payload names a scanlation `team`, never an author or artist. Storing the
                // team as an author would make one placeholder name the strongest signal the
                // recommender sees across every series this group translates.
                authors: Vec::new(),
                status: field(&series, "status")
                    .as_deref()
                    .map_or(SeriesStatus::Unknown, map_status),
                content_type: content_type_of(&series),
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
        let series_path = path.trim_end_matches('/').to_owned();
        parse_blocking(resp, move |root, resp| {
            let payload = flight_payload(root);
            let rows = chapter_rows(&payload)
                .ok_or_else(|| AdapterError::missing("witchtoons chapter list", resp))?;
            let mut chapters = Vec::with_capacity(rows.len());
            for row in &rows {
                let Some(number) = chapter_number(row) else {
                    continue;
                };
                chapters.push(ChapterMeta {
                    number,
                    title: chapter_title(row, number),
                    path: format!("{series_path}/chapter/{}", number_in_url(number)),
                    published_at: field(row, "publishedAt")
                        .or_else(|| field(row, "createdAt"))
                        .and_then(|s| OffsetDateTime::parse(&s, &Rfc3339).ok()),
                    access: access_of(row),
                });
            }
            Ok(chapters)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_CANDIDATES, access_of, chapter_rows, chapter_title, flight_payload, is_series_object,
        is_update_feed, number_in_url, series_path, tags_of, value_matching, values_for_key,
    };
    use crate::types::ChapterAccess;
    use serde_json::json;
    use time::macros::datetime;

    fn payload_of(html: &str) -> String {
        let doc = scraper::Html::parse_document(html);
        flight_payload(doc.root_element())
    }

    /// A flight value routinely straddles two chunks, and the chunks are JSON string literals:
    /// concatenating them raw leaves the escapes unbalanced and the value unparseable, which is
    /// the whole reason each chunk is decoded before being joined.
    #[test]
    fn chunks_are_decoded_before_they_are_joined() {
        let html = r#"<html><body>
            <script>self.__next_f.push([1,"{\"series\":{\"title\":\"A \\\"quo"])</script>
            <script>self.__next_f.push([1,"ted\\\" name\",\"slug\":\"a-name\"}}"])</script>
        </body></html>"#;
        let payload = payload_of(html);
        let series = value_matching(&payload, "series", is_series_object).expect("series object");
        assert_eq!(series["title"], json!("A \"quoted\" name"));
    }

    /// The payload is a stream of props, not a document, so one key carries several unrelated
    /// values. Taking the first match blindly read the home page's carousel as the update feed.
    #[test]
    fn a_repeated_key_is_resolved_by_shape() {
        let payload = r#"{"series":[{"slug":"carousel"}],"series":[{"slug":"feed","lastChapterAt":"2026-08-16T22:43:39.895Z"}]}"#;
        let feed = value_matching(payload, "series", is_update_feed).expect("update feed");
        assert_eq!(feed[0]["slug"], json!("feed"));
    }

    /// A series listed before its first chapter binds `chapters` to `[]`. Read as "not found"
    /// that failed the scan task outright — three of witchtoons' 106 series on the first full
    /// scan — so an announced-but-unreleased series could never finish ingesting. It is an empty
    /// chapter list, and only a payload with no series object at all is a real parse failure.
    #[test]
    fn a_series_with_no_chapters_yet_is_empty_rather_than_missing() {
        let announced =
            r#"{"series":{"title":"Soon","slug":"soon","chapterCount":0},"chapters":[]}"#;
        assert_eq!(chapter_rows(announced), Some(Vec::new()));

        let published = r#"{"series":{"title":"Out","slug":"out"},"chapters":[{"number":1}]}"#;
        assert_eq!(
            chapter_rows(published).map(|rows| rows.len()),
            Some(1),
            "a populated list is still read as itself"
        );

        // Nothing that looks like a series page: the parse failed, and must say so.
        assert_eq!(chapter_rows(r#"{"unrelated":[]}"#), None);
    }

    #[test]
    fn key_lookup_stops_at_the_candidate_cap() {
        let payload = "\"k\":1,".repeat(MAX_CANDIDATES + 5);
        assert_eq!(values_for_key(&payload, "k").len(), MAX_CANDIDATES);
    }

    /// `urlSlug` is what the reader URL uses; `slug` is the fallback for a row that omits it.
    #[test]
    fn a_series_path_prefers_the_url_slug() {
        assert_eq!(
            series_path(&json!({"slug": "internal", "urlSlug": "public"})).as_deref(),
            Some("/series/comic/public")
        );
        assert_eq!(
            series_path(&json!({"slug": "internal"})).as_deref(),
            Some("/series/comic/internal")
        );
        assert_eq!(series_path(&json!({"title": "no slug"})), None);
    }

    /// Prose has no pages to read and the platform does not serve it under the comic prefix, so
    /// a novel row must not become a series at a URL that 404s — the bug `HiveToons` shipped.
    #[test]
    fn a_prose_row_is_not_a_series() {
        assert_eq!(
            series_path(&json!({"slug": "a-novel", "type": "NOVEL"})),
            None
        );
        assert_eq!(
            series_path(&json!({"slug": "a-comic", "type": "MANHUA"})).as_deref(),
            Some("/series/comic/a-comic")
        );
    }

    /// The platform defaults an untitled chapter's `title` to its own number, so storing it
    /// verbatim titled every such chapter with the number already displayed beside it.
    #[test]
    fn a_chapter_titled_with_its_own_number_has_no_title() {
        assert_eq!(chapter_title(&json!({"title": "62"}), 62.0), None);
        assert_eq!(chapter_title(&json!({"title": ""}), 62.0), None);
        assert_eq!(
            chapter_title(&json!({"title": "The Duel"}), 62.0).as_deref(),
            Some("The Duel")
        );
    }

    #[test]
    fn chapter_numbers_render_as_the_reader_url_spells_them() {
        assert_eq!(number_in_url(62.0), "62");
        assert_eq!(number_in_url(62.5), "62.5");
    }

    #[test]
    fn a_locked_chapter_carries_its_unlock_time() {
        assert_eq!(access_of(&json!({"isLocked": false})), ChapterAccess::Free);
        assert_eq!(access_of(&json!({"number": 1})), ChapterAccess::Free);
        assert_eq!(
            access_of(&json!({"isLocked": true, "earlyAccessUntil": "2026-08-20T10:00:00Z"})),
            ChapterAccess::EarlyAccess {
                unlocks_at: Some(datetime!(2026-08-20 10:00:00 UTC))
            }
        );
        // A rule is not a date: a locked chapter with no announced time stays locked.
        assert_eq!(
            access_of(&json!({"isLocked": true, "becomesFreeOnNextRelease": true})),
            ChapterAccess::EarlyAccess { unlocks_at: None }
        );
    }

    #[test]
    fn tags_read_both_payload_shapes_without_duplicating() {
        let value = json!({
            "tags": [{"name": "Rebirth", "slug": "rebirth"}],
            "genres": [{"genre": {"slug": "action"}}, {"genre": {"slug": "rebirth"}}]
        });
        assert_eq!(tags_of(&value), vec!["Rebirth", "action"]);
    }

    /// Every genre this platform publishes carries a trailing emoji. Stored verbatim, `Action ⚔️`
    /// never merges with any other provider's `Action`, and the Discover facet renders the emoji.
    #[test]
    fn genre_names_lose_their_decoration() {
        let value = json!({
            "genres": [
                {"name": "Action \u{2694}\u{fe0f}", "slug": "action"},
                {"name": "Fantasy \u{1f9da}\u{200d}\u{2642}\u{fe0f}", "slug": "fantasy"},
                // A hyphen or a bracket is part of the name, not decoration.
                {"name": "Sci-Fi \u{1f680}", "slug": "sci-fi"},
                {"name": "Slice of Life (Modern)", "slug": "slice-of-life"},
                // Decoration alone leaves nothing, and falls back to the slug.
                {"name": "\u{1f300}", "slug": "isekai"}
            ]
        });
        assert_eq!(
            tags_of(&value),
            vec![
                "Action",
                "Fantasy",
                "Sci-Fi",
                "Slice of Life (Modern)",
                "isekai"
            ]
        );
    }
}
