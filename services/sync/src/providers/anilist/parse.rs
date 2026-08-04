//! `AniList`-shaped response types and the pure functions that build them from GraphQL JSON, kept
//! free of the network so the response shaping is directly testable. Both types convert into the
//! provider-agnostic `RemoteEntry`/`RemoteMetadata` via `From`.

use time::OffsetDateTime;

use tankovault_db::repo::catalog::MIN_TAG_WEIGHT;
use tankovault_domain::{ContentType, SeriesStatus};

use crate::mapping::{AniListStatus, content_type_from_origin, series_status_from_media};
use crate::provider::{RemoteEntry, RemoteMetadata, RemoteTag};

/// One entry from a user's `AniList` manga list: the reader's own position on the list, plus
/// the catalogue metadata of the work it points at. `media` is parsed by the same function as
/// the tokenless public lookup, so the two paths can't drift into disagreeing about it.
#[derive(Debug, Clone)]
pub(crate) struct AniListEntry {
    pub(crate) status: AniListStatus,
    pub(crate) progress: f64,
    pub(crate) updated_at: OffsetDateTime,
    pub(crate) media: MediaMetadata,
}

impl From<AniListEntry> for RemoteEntry {
    fn from(e: AniListEntry) -> Self {
        Self {
            status: e.status.to_watch_status(),
            progress: e.progress,
            updated_at: e.updated_at,
            metadata: e.media.into(),
        }
    }
}

/// Public catalogue metadata for one `AniList` media. Reachable with **no** user token from the
/// public GraphQL API, and carried on every list entry as well.
#[derive(Debug, Clone)]
pub(crate) struct MediaMetadata {
    pub(crate) media_id: i64,
    /// All titles (romaji/english/native, then every synonym), non-blank only. `titles[0]` is
    /// always the first non-empty of romaji/english/native, so callers relying on "the primary
    /// title" (e.g. the remote-entry snapshot) see a stable value.
    pub(crate) titles: Vec<String>,
    pub(crate) description: Option<String>,
    pub(crate) cover_url: Option<String>,
    pub(crate) start_year: Option<i32>,
    pub(crate) content_type: ContentType,
    /// The work's *publication* status, not the reader's list status.
    pub(crate) series_status: SeriesStatus,
    /// Genres, used as an extra local-matching signal alongside title, and persisted as tags.
    pub(crate) tags: Vec<String>,
    /// `AniList`'s descriptive tag vocabulary, each with its rank as a weight. Kept apart from
    /// `tags` because only the coarse genres feed the matcher: a local candidate carries the
    /// four genres an adapter scraped, so scoring it against twenty-five themes it could never
    /// have would read as disagreement.
    pub(crate) themes: Vec<RemoteTag>,
    /// Staff names (story/art credits), matched against locally-scraped authors.
    pub(crate) authors: Vec<String>,
    pub(crate) is_adult: bool,
    pub(crate) average_score: Option<f32>,
    pub(crate) popularity: Option<i32>,
    /// What the work was adapted from, lower-cased (`original`, `light_novel`, …).
    pub(crate) source: Option<String>,
}

impl From<MediaMetadata> for RemoteMetadata {
    fn from(m: MediaMetadata) -> Self {
        Self {
            external_id: m.media_id.to_string(),
            titles: m.titles,
            description: m.description,
            cover_url: m.cover_url,
            start_year: m.start_year,
            content_type: m.content_type,
            series_status: m.series_status,
            tags: m.tags,
            themes: m.themes,
            authors: m.authors,
            is_adult: Some(m.is_adult),
            external_score: m.average_score,
            external_popularity: m.popularity,
            external_source: m.source,
        }
    }
}

/// Extract [`AniListEntry`]s from a `MediaListCollection` GraphQL `data` object. Entries
/// with an unrecognised status or no usable title are skipped rather than failing the run.
#[must_use]
pub(crate) fn parse_media_list(data: &serde_json::Value) -> Vec<AniListEntry> {
    let Some(lists) = data
        .get("MediaListCollection")
        .and_then(|c| c.get("lists"))
        .and_then(serde_json::Value::as_array)
    else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for list in lists {
        let Some(entries) = list.get("entries").and_then(serde_json::Value::as_array) else {
            continue;
        };
        for entry in entries {
            if let Some(parsed) = parse_entry(entry) {
                out.push(parsed);
            }
        }
    }
    out
}

/// Whether the `MediaListCollection` in this response reports a further chunk to fetch.
/// A missing flag is treated as "no more chunks" so pagination terminates safely.
#[must_use]
pub(crate) fn has_next_chunk(data: &serde_json::Value) -> bool {
    data.get("MediaListCollection")
        .and_then(|c| c.get("hasNextChunk"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

fn parse_entry(entry: &serde_json::Value) -> Option<AniListEntry> {
    let media = parse_media(entry.get("media")?)?;
    let status = AniListStatus::parse(entry.get("status").and_then(serde_json::Value::as_str)?)?;

    let progress = entry
        .get("progress")
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(0.0);
    let updated_at = entry
        .get("updatedAt")
        .and_then(serde_json::Value::as_i64)
        .and_then(|s| OffsetDateTime::from_unix_timestamp(s).ok())
        .unwrap_or(OffsetDateTime::UNIX_EPOCH);

    Some(AniListEntry {
        status,
        progress,
        updated_at,
        media,
    })
}

/// Extract [`MediaMetadata`] from a public `Media(...)` GraphQL `data` object (no user token).
/// Returns `None` when there is no `Media` node or it has no usable id/title.
#[must_use]
pub(crate) fn parse_media_metadata(data: &serde_json::Value) -> Option<MediaMetadata> {
    parse_media(data.get("Media")?)
}

/// Read one `media` node — the single place a GraphQL media selection becomes local values,
/// shared by the list and public-metadata parsers so neither can drift from the other. Returns
/// `None` for a node with no id or usable title: an entry we can't name, we can't match.
fn parse_media(media: &serde_json::Value) -> Option<MediaMetadata> {
    let media_id = media.get("id").and_then(serde_json::Value::as_i64)?;
    let titles = titles_from_media(media);
    if titles.is_empty() {
        return None;
    }
    // AniList wraps descriptions in light HTML (<br>, <i>, ...) even with asHtml:false in
    // some cases; strip tags so the stored description is plain text.
    let description = media
        .get("description")
        .and_then(serde_json::Value::as_str)
        .map(strip_html)
        .filter(|s| !s.trim().is_empty());
    let cover_url = media
        .get("coverImage")
        .and_then(|c| {
            c.get("extraLarge")
                .and_then(serde_json::Value::as_str)
                .or_else(|| c.get("large").and_then(serde_json::Value::as_str))
        })
        .map(str::to_owned)
        .filter(|s| !s.trim().is_empty());
    Some(MediaMetadata {
        media_id,
        titles,
        description,
        cover_url,
        start_year: start_year_from_media(media),
        content_type: content_type_from_media(media),
        series_status: series_status_from_media(
            media.get("status").and_then(serde_json::Value::as_str),
        ),
        tags: genres_from_media(media),
        themes: themes_from_media(media),
        authors: staff_from_media(media),
        is_adult: media
            .get("isAdult")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        // Clamped before the cast, not after: `series_external_score_check` bounds the column to
        // 0..100 and an out-of-range value would abort the whole enrichment transaction.
        average_score: media
            .get("averageScore")
            .and_then(serde_json::Value::as_i64)
            .and_then(|s| u8::try_from(s.clamp(0, 100)).ok())
            .map(f32::from),
        popularity: media
            .get("popularity")
            .and_then(serde_json::Value::as_i64)
            .and_then(|p| i32::try_from(p).ok()),
        source: media
            .get("source")
            .and_then(serde_json::Value::as_str)
            .map(str::to_lowercase)
            .filter(|s| !s.is_empty()),
    })
}

/// Candidate titles for a `media` object: the non-blank romaji/english/native trio first (so
/// `titles[0]` is a stable "primary"), then every non-blank synonym `AniList` tracks.
fn titles_from_media(media: &serde_json::Value) -> Vec<String> {
    let mut titles = Vec::new();
    if let Some(title) = media.get("title") {
        for key in ["romaji", "english", "native"] {
            if let Some(t) = title.get(key).and_then(serde_json::Value::as_str)
                && !t.trim().is_empty()
            {
                titles.push(t.to_owned());
            }
        }
    }
    if let Some(synonyms) = media.get("synonyms").and_then(serde_json::Value::as_array) {
        for s in synonyms {
            if let Some(t) = s.as_str()
                && !t.trim().is_empty()
            {
                titles.push(t.to_owned());
            }
        }
    }
    titles
}

/// Publication start year, when `AniList` knows one.
fn start_year_from_media(media: &serde_json::Value) -> Option<i32> {
    media
        .get("startDate")
        .and_then(|d| d.get("year"))
        .and_then(serde_json::Value::as_i64)
        .and_then(|y| i32::try_from(y).ok())
}

/// Manga/manhwa/manhua, from `AniList`'s country of origin with its format as the fallback.
fn content_type_from_media(media: &serde_json::Value) -> ContentType {
    content_type_from_origin(
        media
            .get("countryOfOrigin")
            .and_then(serde_json::Value::as_str),
        media.get("format").and_then(serde_json::Value::as_str),
    )
}

/// Genre names for a `media` object (empty when absent).
fn genres_from_media(media: &serde_json::Value) -> Vec<String> {
    media
        .get("genres")
        .and_then(serde_json::Value::as_array)
        .map(|genres| {
            genres
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// `AniList`'s descriptive tags for a `media` object, weighted by rank (empty when absent).
///
/// **Spoiler- and adult-flagged tags are dropped here and never stored.** `series_tags` has no
/// visibility column, and every path that reads it — the series page's tag chips, the browse
/// filters, the matcher's overlap score, and the recommender's own "because you liked X, which
/// shares …" explanation — renders what it finds. A tag naming a late plot twist or an explicit
/// act would reach a reader through any one of them, and a filter that has to be remembered in
/// five places is a filter that will be forgotten in one. The recall this costs is real and is
/// the price of not needing that audit. Restoring them means a visibility column plus a
/// predicate on every one of those reads, not a change here.
fn themes_from_media(media: &serde_json::Value) -> Vec<RemoteTag> {
    let Some(tags) = media.get("tags").and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(tags.len());
    for tag in tags {
        let flagged = |key: &str| tag.get(key).and_then(serde_json::Value::as_bool) == Some(true);
        if flagged("isMediaSpoiler") || flagged("isAdult") {
            continue;
        }
        let Some(name) = tag.get("name").and_then(serde_json::Value::as_str) else {
            continue;
        };
        if name.trim().is_empty() {
            continue;
        }
        // A rank of zero is a term nobody upvoted, and absent is the same statement. Both floor
        // rather than drop: the tag is still evidence, just the weakest kind the column allows.
        let rank = tag
            .get("rank")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0)
            .clamp(0, 100);
        let weight = (f32::from(u8::try_from(rank).unwrap_or(0)) / 100.0).max(MIN_TAG_WEIGHT);
        out.push(RemoteTag {
            name: name.to_owned(),
            weight,
        });
    }
    out
}

/// Staff (story/art) credit names for a `media` object (empty when absent).
fn staff_from_media(media: &serde_json::Value) -> Vec<String> {
    media
        .get("staff")
        .and_then(|s| s.get("edges"))
        .and_then(serde_json::Value::as_array)
        .map(|edges| {
            edges
                .iter()
                .filter_map(|e| e.get("node")?.get("name")?.get("full")?.as_str())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Strip HTML tags and collapse the common `<br>` breaks `AniList` uses in descriptions to
/// plain text with normal line breaks. Deliberately tiny — `AniList` descriptions only ever
/// carry simple inline markup, not arbitrary HTML.
fn strip_html(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_tag = false;
    for ch in input.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.trim().to_owned()
}

#[cfg(test)]
mod tests {
    // Tests assert exact equality of small, exactly-representable progress/id/weight values.
    // A tag weight is `rank / 100.0`, and IEEE division is correctly rounded, so it is bit-equal
    // to the decimal literal the assertion spells.
    #![expect(
        clippy::float_cmp,
        reason = "parsed progress and tag-weight values are compared against the exact numbers \
                  the fixture documents encode"
    )]

    use super::{
        AniListStatus, ContentType, MIN_TAG_WEIGHT, SeriesStatus, has_next_chunk, parse_media_list,
        parse_media_metadata, strip_html,
    };

    #[test]
    fn parses_a_media_list_collection() {
        let data = serde_json::json!({
            "MediaListCollection": { "lists": [
                { "entries": [
                    {
                        "status": "CURRENT", "progress": 42, "updatedAt": 1_700_000_000i64,
                        "media": {
                            "id": 105_778, "countryOfOrigin": "KR",
                            "status": "FINISHED", "format": "MANGA",
                            "startDate": { "year": 2018 },
                            "description": "A hunter <b>rises</b>.",
                            "coverImage": { "extraLarge": "https://img/xl.jpg" },
                            "title": { "romaji": "Na Honjaman Level Up",
                                       "english": "Solo Leveling", "native": null },
                            "synonyms": ["Only I Level Up", "I Level Up Alone"],
                            "genres": ["Action", "Fantasy"],
                            "staff": { "edges": [
                                { "node": { "name": { "full": "Chugong" } } },
                                { "node": { "name": { "full": "Redice Studio" } } }
                            ] }
                        }
                    },
                    {
                        "status": "PLANNING", "progress": 0, "updatedAt": 1_700_000_500i64,
                        "media": {
                            "id": 30_002, "countryOfOrigin": "JP",
                            "startDate": { "year": null },
                            "title": { "romaji": "Berserk", "english": null, "native": "ベルセルク" }
                        }
                    }
                ] }
            ] }
        });

        let entries = parse_media_list(&data);
        assert_eq!(entries.len(), 2);

        let solo = &entries[0];
        assert_eq!(solo.status, AniListStatus::Current);
        assert_eq!(solo.progress, 42.0);
        assert_eq!(solo.media.media_id, 105_778);
        assert_eq!(solo.media.start_year, Some(2018));
        assert_eq!(solo.media.content_type, ContentType::Manhwa);
        assert_eq!(
            solo.media.titles,
            vec![
                "Na Honjaman Level Up",
                "Solo Leveling",
                "Only I Level Up",
                "I Level Up Alone",
            ]
        );
        assert_eq!(solo.media.tags, vec!["Action", "Fantasy"]);
        assert_eq!(solo.media.authors, vec!["Chugong", "Redice Studio"]);
        // A list entry carries the full media metadata, not just matcher-scored fields.
        assert_eq!(solo.media.description.as_deref(), Some("A hunter rises."));
        assert_eq!(solo.media.cover_url.as_deref(), Some("https://img/xl.jpg"));
        assert_eq!(solo.media.series_status, SeriesStatus::Completed);

        let berserk = &entries[1];
        assert_eq!(berserk.media.content_type, ContentType::Manga);
        assert_eq!(berserk.media.start_year, None);
        // No `synonyms` field at all in this fixture: falls back to just romaji/native,
        // not a parse failure.
        assert_eq!(berserk.media.titles, vec!["Berserk", "ベルセルク"]);
        // No genres/staff in the fixture: both default to empty, not a parse failure.
        assert!(berserk.media.tags.is_empty());
        assert!(berserk.media.authors.is_empty());
        // A missing publication status must stay `Unknown`, not default to a real state.
        assert_eq!(berserk.media.series_status, SeriesStatus::Unknown);
        assert_eq!(berserk.media.description, None);
        assert_eq!(berserk.media.cover_url, None);
    }

    #[test]
    fn blank_and_non_string_synonyms_are_skipped() {
        let data = serde_json::json!({
            "MediaListCollection": { "lists": [
                { "entries": [
                    { "status": "CURRENT", "progress": 1, "updatedAt": 1,
                      "media": { "id": 1, "title": { "romaji": "x" },
                                 "synonyms": ["  ", "Valid Synonym", null, 5] } }
                ] }
            ] }
        });
        let entries = parse_media_list(&data);
        assert_eq!(entries[0].media.titles, vec!["x", "Valid Synonym"]);
    }

    #[test]
    fn skips_entries_without_title_or_status() {
        let data = serde_json::json!({
            "MediaListCollection": { "lists": [
                { "entries": [
                    { "status": "WHAT", "progress": 1, "updatedAt": 1,
                      "media": { "id": 1, "title": { "romaji": "x" } } },
                    { "status": "CURRENT", "progress": 1, "updatedAt": 1,
                      "media": { "id": 2, "title": { "romaji": null, "english": "", "native": null } } }
                ] }
            ] }
        });
        assert!(parse_media_list(&data).is_empty());
    }

    #[test]
    fn empty_or_missing_collection_yields_no_entries() {
        assert!(parse_media_list(&serde_json::json!({})).is_empty());
        assert!(
            parse_media_list(&serde_json::json!({
                "MediaListCollection": { "lists": [] }
            }))
            .is_empty()
        );
    }

    #[test]
    fn has_next_chunk_reads_the_flag() {
        assert!(has_next_chunk(&serde_json::json!({
            "MediaListCollection": { "hasNextChunk": true, "lists": [] }
        })));
        assert!(!has_next_chunk(&serde_json::json!({
            "MediaListCollection": { "hasNextChunk": false, "lists": [] }
        })));
        // A missing flag terminates pagination.
        assert!(!has_next_chunk(&serde_json::json!({
            "MediaListCollection": { "lists": [] }
        })));
        assert!(!has_next_chunk(&serde_json::json!({})));
    }

    #[test]
    fn parses_public_media_metadata() {
        let data = serde_json::json!({
            "Media": {
                "id": 105_778, "countryOfOrigin": "KR",
                "status": "RELEASING", "format": "MANGA",
                "startDate": { "year": 2018 },
                "description": "A hunter <b>rises</b>.<br>Solo.",
                "coverImage": { "extraLarge": "https://img/xl.jpg", "large": "https://img/l.jpg" },
                "title": { "romaji": "Na Honjaman Level Up",
                           "english": "Solo Leveling", "native": null },
                "synonyms": ["Only I Level Up"],
                "genres": ["Action", "Fantasy"],
                "staff": { "edges": [ { "node": { "name": { "full": "Chugong" } } } ] }
            }
        });
        let m = parse_media_metadata(&data).expect("metadata");
        assert_eq!(m.media_id, 105_778);
        assert_eq!(m.content_type, ContentType::Manhwa);
        assert_eq!(m.start_year, Some(2018));
        assert_eq!(
            m.titles,
            vec!["Na Honjaman Level Up", "Solo Leveling", "Only I Level Up"]
        );
        // HTML stripped, prefers extraLarge cover.
        assert_eq!(m.description.as_deref(), Some("A hunter rises.Solo."));
        assert_eq!(m.cover_url.as_deref(), Some("https://img/xl.jpg"));
        assert_eq!(m.tags, vec!["Action", "Fantasy"]);
        assert_eq!(m.authors, vec!["Chugong"]);
        assert_eq!(m.series_status, SeriesStatus::Ongoing);
    }

    #[test]
    fn public_metadata_missing_or_titleless_is_none() {
        assert!(parse_media_metadata(&serde_json::json!({})).is_none());
        assert!(
            parse_media_metadata(&serde_json::json!({
                "Media": { "id": 1, "title": { "romaji": null } }
            }))
            .is_none()
        );
    }

    #[test]
    fn public_metadata_falls_back_to_large_cover_and_blank_description() {
        let data = serde_json::json!({
            "Media": {
                "id": 7, "countryOfOrigin": "JP",
                "description": "   ",
                "coverImage": { "large": "https://img/l.jpg" },
                "title": { "romaji": "Berserk" }
            }
        });
        let m = parse_media_metadata(&data).expect("metadata");
        assert_eq!(m.description, None);
        assert_eq!(m.cover_url.as_deref(), Some("https://img/l.jpg"));
        assert_eq!(m.content_type, ContentType::Manga);
    }

    /// A media node carrying every widened recommendation signal.
    fn signal_rich_media() -> serde_json::Value {
        serde_json::json!({
            "Media": {
                "id": 105_778, "countryOfOrigin": "KR",
                "title": { "romaji": "Na Honjaman Level Up" },
                "genres": ["Action", "Fantasy"],
                "tags": [
                    { "name": "Regression", "rank": 87, "isMediaSpoiler": false, "isAdult": false },
                    { "name": "Dungeon", "rank": 60, "isMediaSpoiler": false, "isAdult": false },
                    { "name": "Dies Halfway", "rank": 95, "isMediaSpoiler": true, "isAdult": false },
                    { "name": "Nudity", "rank": 70, "isMediaSpoiler": false, "isAdult": true },
                    { "name": "Nobody Voted For This", "rank": 0 },
                    { "name": "Rankless" },
                    { "name": "   ", "rank": 50 }
                ],
                "averageScore": 84, "popularity": 250_000,
                "isAdult": false, "source": "WEB_NOVEL"
            }
        })
    }

    /// Genres and the rich tag vocabulary must stay apart all the way out of the parser.
    ///
    /// The bug this pins: folding `tags` into `genres` hands the matcher twenty-five terms a
    /// locally-scraped candidate could never carry, so the overlap score reads as disagreement
    /// and confident matches stop resolving.
    #[test]
    fn genres_and_rich_tags_stay_separate() {
        let m = parse_media_metadata(&signal_rich_media()).expect("metadata");
        assert_eq!(m.tags, vec!["Action", "Fantasy"]);
        let themes: Vec<&str> = m.themes.iter().map(|t| t.name.as_str()).collect();
        assert!(
            !themes.contains(&"Action"),
            "a genre must not become a theme"
        );
    }

    /// A rich tag's rank becomes its link weight, and a rank of zero is floored, never dropped
    /// and never zero.
    ///
    /// The bug this pins: `series_tags_weight_check` rejects `weight <= 0`, so a literal
    /// `rank / 100` of zero aborts the transaction that carried the whole enrichment batch —
    /// one unvoted tag silently costing a series every other signal in the same sweep.
    #[test]
    fn a_rank_becomes_a_weight_and_zero_is_floored() {
        let m = parse_media_metadata(&signal_rich_media()).expect("metadata");
        let weight_of = |name: &str| {
            m.themes
                .iter()
                .find(|t| t.name == name)
                .unwrap_or_else(|| panic!("{name} is missing"))
                .weight
        };
        assert_eq!(weight_of("Regression"), 0.87);
        assert_eq!(weight_of("Dungeon"), 0.60);
        assert_eq!(weight_of("Nobody Voted For This"), MIN_TAG_WEIGHT);
        // An absent rank is the same statement as a zero one.
        assert_eq!(weight_of("Rankless"), MIN_TAG_WEIGHT);
        assert!(
            !m.themes.iter().any(|t| t.name.trim().is_empty()),
            "a blank tag name has no slug and would be dropped downstream anyway"
        );
    }

    /// Spoiler- and adult-flagged tags are dropped by the parser, not filtered later.
    ///
    /// The bug this pins: `series_tags` has no visibility column, so a stored spoiler tag is
    /// rendered by the series page, the browse filters and the recommender's explanation alike.
    /// Every one of those would have to remember to exclude it, and the one that forgets spoils
    /// the work for a reader who never asked.
    #[test]
    fn spoiler_and_adult_tags_are_never_stored() {
        let m = parse_media_metadata(&signal_rich_media()).expect("metadata");
        let themes: Vec<&str> = m.themes.iter().map(|t| t.name.as_str()).collect();
        assert!(
            !themes.contains(&"Dies Halfway"),
            "spoiler tag leaked: {themes:?}"
        );
        assert!(!themes.contains(&"Nudity"), "adult tag leaked: {themes:?}");
        assert_eq!(
            themes,
            vec!["Regression", "Dungeon", "Nobody Voted For This", "Rankless"]
        );
    }

    #[test]
    fn appeal_signals_and_the_adult_flag_are_parsed() {
        let m = parse_media_metadata(&signal_rich_media()).expect("metadata");
        assert!(!m.is_adult);
        assert_eq!(m.average_score, Some(84.0));
        assert_eq!(m.popularity, Some(250_000));
        assert_eq!(m.source.as_deref(), Some("web_novel"));
    }

    /// Absent signals must stay absent rather than defaulting to a fabricated value.
    ///
    /// The bug this pins: a score of `0` for "nobody has scored this" makes the appeal prior
    /// read an unrated series as the worst in the catalogue.
    #[test]
    fn missing_signals_are_none_and_a_missing_adult_flag_is_false() {
        let data = serde_json::json!({
            "Media": { "id": 7, "title": { "romaji": "Berserk" } }
        });
        let m = parse_media_metadata(&data).expect("metadata");
        assert!(m.themes.is_empty());
        assert_eq!(m.average_score, None);
        assert_eq!(m.popularity, None);
        assert_eq!(m.source, None);
        assert!(!m.is_adult);
    }

    /// An out-of-range score is clamped, because `series_external_score_check` bounds the column
    /// to 0..100 and a rejected write loses the whole enrichment batch.
    #[test]
    fn an_out_of_range_score_is_clamped_rather_than_rejected() {
        let over = serde_json::json!({
            "Media": { "id": 1, "title": { "romaji": "x" }, "averageScore": 4242 }
        });
        assert_eq!(
            parse_media_metadata(&over).expect("metadata").average_score,
            Some(100.0)
        );
        let under = serde_json::json!({
            "Media": { "id": 1, "title": { "romaji": "x" }, "averageScore": -5 }
        });
        assert_eq!(
            parse_media_metadata(&under)
                .expect("metadata")
                .average_score,
            Some(0.0)
        );
    }

    #[test]
    fn strip_html_removes_tags_and_trims() {
        assert_eq!(strip_html("<p>hello <i>world</i></p>"), "hello world");
        assert_eq!(strip_html("  plain  "), "plain");
    }
}
