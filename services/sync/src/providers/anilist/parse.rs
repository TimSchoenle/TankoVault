//! `AniList`-shaped response types and the pure functions that build them from GraphQL JSON.
//!
//! Nothing here touches the network, so the response shaping that is easy to get wrong is
//! directly testable — which is why the whole of this module's test coverage is at the bottom
//! of this file rather than behind a client. Both types convert into the provider-agnostic
//! `RemoteEntry`/`RemoteMetadata` via `From`, so `crate::engine` never sees an `AniList` type.

use time::OffsetDateTime;

use tankovault_domain::ContentType;

use crate::mapping::{AniListStatus, content_type_from_country};
use crate::provider::{RemoteEntry, RemoteMetadata};

/// One entry from a user's `AniList` manga list, `AniList`-shaped (numeric id, `AniList`'s own
/// status vocabulary).
#[derive(Debug, Clone)]
pub(crate) struct AniListEntry {
    pub(crate) media_id: i64,
    /// Candidate titles (romaji/english/native, then every `AniList` synonym), non-empty
    /// ones only. `titles[0]` is always the first non-empty of romaji/english/native, so
    /// callers relying on "the primary title" (e.g. the remote-entry snapshot) still see
    /// the same value as before synonyms were added.
    pub(crate) titles: Vec<String>,
    pub(crate) status: AniListStatus,
    pub(crate) progress: f64,
    pub(crate) updated_at: OffsetDateTime,
    pub(crate) start_year: Option<i32>,
    pub(crate) content_type: ContentType,
    /// Genres, used as an extra local-matching signal alongside title (design: make
    /// `AniList` matching use the metadata adapters now capture).
    pub(crate) tags: Vec<String>,
    /// Staff names (story/art credits), matched against locally-scraped authors.
    pub(crate) authors: Vec<String>,
}

impl From<AniListEntry> for RemoteEntry {
    fn from(e: AniListEntry) -> Self {
        Self {
            external_id: e.media_id.to_string(),
            titles: e.titles,
            status: e.status.to_watch_status(),
            progress: e.progress,
            updated_at: e.updated_at,
            start_year: e.start_year,
            content_type: e.content_type,
            tags: e.tags,
            authors: e.authors,
        }
    }
}

/// Public catalogue metadata for one `AniList` media, fetched **without** any user token
/// from the public GraphQL API.
#[derive(Debug, Clone)]
pub(crate) struct MediaMetadata {
    pub(crate) media_id: i64,
    /// All titles (romaji/english/native, then every synonym), non-blank only.
    pub(crate) titles: Vec<String>,
    pub(crate) description: Option<String>,
    pub(crate) cover_url: Option<String>,
    pub(crate) start_year: Option<i32>,
    pub(crate) content_type: ContentType,
    pub(crate) tags: Vec<String>,
    pub(crate) authors: Vec<String>,
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
            tags: m.tags,
            authors: m.authors,
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
    let media = entry.get("media")?;
    let media_id = media.get("id").and_then(serde_json::Value::as_i64)?;
    let status = AniListStatus::parse(entry.get("status").and_then(serde_json::Value::as_str)?)?;

    let titles = titles_from_media(media);
    if titles.is_empty() {
        return None;
    }

    let progress = entry
        .get("progress")
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(0.0);
    let updated_at = entry
        .get("updatedAt")
        .and_then(serde_json::Value::as_i64)
        .and_then(|s| OffsetDateTime::from_unix_timestamp(s).ok())
        .unwrap_or(OffsetDateTime::UNIX_EPOCH);
    let start_year = start_year_from_media(media);
    let content_type = content_type_from_media(media);

    let tags = genres_from_media(media);
    let authors = staff_from_media(media);

    Some(AniListEntry {
        media_id,
        titles,
        status,
        progress,
        updated_at,
        start_year,
        content_type,
        tags,
        authors,
    })
}

/// Extract [`MediaMetadata`] from a public `Media(...)` GraphQL `data` object (no user
/// token). Returns `None` when there is no `Media` node or it has no usable id/title.
#[must_use]
pub(crate) fn parse_media_metadata(data: &serde_json::Value) -> Option<MediaMetadata> {
    let media = data.get("Media")?;
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
        tags: genres_from_media(media),
        authors: staff_from_media(media),
    })
}

/// Candidate titles for a `media` object: the non-blank romaji/english/native trio first
/// (so `titles[0]` is a stable "primary"), then every non-blank synonym `AniList` tracks
/// (abbreviations, fan-translation names, other-language releases). Shared by the list
/// parser and the public-metadata parser so both capture the full alternative-name set.
fn titles_from_media(media: &serde_json::Value) -> Vec<String> {
    let mut titles = Vec::new();
    if let Some(title) = media.get("title") {
        for key in ["romaji", "english", "native"] {
            if let Some(t) = title.get(key).and_then(serde_json::Value::as_str) {
                if !t.trim().is_empty() {
                    titles.push(t.to_owned());
                }
            }
        }
    }
    if let Some(synonyms) = media.get("synonyms").and_then(serde_json::Value::as_array) {
        for s in synonyms {
            if let Some(t) = s.as_str() {
                if !t.trim().is_empty() {
                    titles.push(t.to_owned());
                }
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

/// Manga/manhwa/manhua, inferred from `AniList`'s country of origin.
fn content_type_from_media(media: &serde_json::Value) -> ContentType {
    content_type_from_country(
        media
            .get("countryOfOrigin")
            .and_then(serde_json::Value::as_str),
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
    // Tests assert exact equality of small, exactly-representable progress/id values.
    #![allow(clippy::float_cmp)]

    use super::{
        AniListStatus, ContentType, has_next_chunk, parse_media_list, parse_media_metadata,
        strip_html,
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
                            "startDate": { "year": 2018 },
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
        assert_eq!(solo.media_id, 105_778);
        assert_eq!(solo.status, AniListStatus::Current);
        assert_eq!(solo.progress, 42.0);
        assert_eq!(solo.start_year, Some(2018));
        assert_eq!(solo.content_type, ContentType::Manhwa);
        assert_eq!(
            solo.titles,
            vec![
                "Na Honjaman Level Up",
                "Solo Leveling",
                "Only I Level Up",
                "I Level Up Alone",
            ]
        );
        assert_eq!(solo.tags, vec!["Action", "Fantasy"]);
        assert_eq!(solo.authors, vec!["Chugong", "Redice Studio"]);

        let berserk = &entries[1];
        assert_eq!(berserk.content_type, ContentType::Manga);
        assert_eq!(berserk.start_year, None);
        // No `synonyms` field at all in this fixture: falls back to just romaji/native,
        // not a parse failure.
        assert_eq!(berserk.titles, vec!["Berserk", "ベルセルク"]);
        // No genres/staff in the fixture: both default to empty, not a parse failure.
        assert!(berserk.tags.is_empty());
        assert!(berserk.authors.is_empty());
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
        assert_eq!(entries[0].titles, vec!["x", "Valid Synonym"]);
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

    #[test]
    fn strip_html_removes_tags_and_trims() {
        assert_eq!(strip_html("<p>hello <i>world</i></p>"), "hello world");
        assert_eq!(strip_html("  plain  "), "plain");
    }
}
