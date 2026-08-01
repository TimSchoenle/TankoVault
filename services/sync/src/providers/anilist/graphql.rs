//! The GraphQL operations this service issues against `AniList`. Each query/mutation document
//! sits immediately above the one method that sends it, so a field added to one and a field read
//! from the response are visible together.

use anyhow::anyhow;
use secrecy::SecretString;
use tracing::info;

use crate::mapping::AniListStatus;
use crate::provider::Viewer;

use super::client::AniListClient;
use super::parse::{
    AniListEntry, MediaMetadata, has_next_chunk, parse_media_list, parse_media_metadata,
};

/// `AniList` returns a user's list in chunks and caps `perChunk` at 500.
const PER_CHUNK: i64 = 500;

/// The authenticated viewer's id and display name.
const VIEWER_QUERY: &str = "query { Viewer { id name } }";

/// One page of the viewer's manga list.
///
/// The `media` selection deliberately matches [`METADATA_QUERY`] below, not a narrower
/// matcher-only set, so a list sync can fold full metadata into the matched series for free.
const MEDIA_LIST_QUERY: &str = "\
    query ($userId: Int, $chunk: Int, $perChunk: Int) { \
      MediaListCollection(userId: $userId, type: MANGA, chunk: $chunk, perChunk: $perChunk) { \
        hasNextChunk \
        lists { entries { \
          status progress updatedAt \
          media { id countryOfOrigin format status startDate { year } \
                  description(asHtml: false) \
                  coverImage { extraLarge large } \
                  title { romaji english native } \
                  synonyms \
                  genres \
                  staff(sort: RELEVANCE, perPage: 5) { edges { node { name { full } } } } } \
        } } \
      } \
    }";

/// Create or update one remote list entry.
const SAVE_ENTRY_MUTATION: &str = "\
    mutation ($mediaId: Int, $status: MediaListStatus, $progress: Int) { \
      SaveMediaListEntry(mediaId: $mediaId, status: $status, progress: $progress) { id } \
    }";

/// Title search, used to resolve a local series to an `AniList` media id.
const SEARCH_QUERY: &str = "query ($search: String) { Media(search: $search, type: MANGA) { id } }";

/// The full public media-metadata fragment (no user token required). Shared by the id- and
/// title-keyed public lookups, which differ only in which variable they bind.
const METADATA_QUERY: &str = "\
    query ($id: Int, $search: String) { \
      Media(id: $id, search: $search, type: MANGA) { \
        id countryOfOrigin format status startDate { year } \
        description(asHtml: false) \
        coverImage { extraLarge large } \
        title { romaji english native } \
        synonyms \
        genres \
        staff(sort: RELEVANCE, perPage: 5) { edges { node { name { full } } } } \
      } \
    }";

impl AniListClient {
    /// Resolve the authenticated viewer's `AniList` user id and display name (the latter is
    /// cached against the linked account so the UI can show "Connected as X").
    pub(crate) async fn viewer(&self, access_token: &SecretString) -> anyhow::Result<Viewer> {
        let data = self
            .graphql(access_token, VIEWER_QUERY, serde_json::json!({}))
            .await?;
        let viewer = data
            .get("Viewer")
            .ok_or_else(|| anyhow!("AniList Viewer query returned no data"))?;
        let id = viewer
            .get("id")
            .and_then(serde_json::Value::as_i64)
            .ok_or_else(|| anyhow!("AniList Viewer query returned no id"))?;
        let name = viewer
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned();
        Ok(Viewer {
            id: id.to_string(),
            name,
        })
    }

    /// Fetch the viewer's full manga list.
    ///
    /// A large list spans several responses, so every chunk is paged through via the
    /// `hasNextChunk` flag and the results concatenated — the whole list is returned rather
    /// than only its first page.
    pub(crate) async fn fetch_media_list(
        &self,
        access_token: &SecretString,
        user_id: i64,
    ) -> anyhow::Result<Vec<AniListEntry>> {
        let mut all = Vec::new();
        let mut chunk = 1;
        loop {
            let data = self
                .graphql(
                    access_token,
                    MEDIA_LIST_QUERY,
                    serde_json::json!({
                        "userId": user_id,
                        "chunk": chunk,
                        "perChunk": PER_CHUNK,
                    }),
                )
                .await?;
            all.extend(parse_media_list(&data));
            if !has_next_chunk(&data) {
                break;
            }
            chunk += 1;
        }
        info!("Fetched all manga list {}", all.len());
        Ok(all)
    }

    /// Create or update a remote list entry.
    pub(crate) async fn save_entry(
        &self,
        access_token: &SecretString,
        media_id: i64,
        status: AniListStatus,
        progress: i64,
    ) -> anyhow::Result<()> {
        let vars = serde_json::json!({
            "mediaId": media_id,
            "status": status.as_graphql(),
            "progress": progress,
        });
        self.graphql(access_token, SAVE_ENTRY_MUTATION, vars)
            .await?;
        Ok(())
    }

    /// Best-effort search for a manga's `AniList` media id by title.
    pub(crate) async fn search_media(
        &self,
        access_token: &SecretString,
        title: &str,
    ) -> anyhow::Result<Option<i64>> {
        // A no-match search yields a GraphQL error; treat that as "not found".
        let Ok(data) = self
            .graphql(
                access_token,
                SEARCH_QUERY,
                serde_json::json!({ "search": title }),
            )
            .await
        else {
            return Ok(None);
        };
        Ok(data
            .get("Media")
            .and_then(|m| m.get("id"))
            .and_then(serde_json::Value::as_i64))
    }

    /// Fetch public metadata for a known `AniList` media id — **no user token**. Returns
    /// `None` when the id no longer resolves.
    pub(crate) async fn fetch_metadata_by_id(
        &self,
        media_id: i64,
    ) -> anyhow::Result<Option<MediaMetadata>> {
        let Ok(data) = self
            .graphql_public(METADATA_QUERY, serde_json::json!({ "id": media_id }))
            .await
        else {
            return Ok(None);
        };
        Ok(parse_media_metadata(&data))
    }

    /// Fetch public metadata for a work by title — **no user token**. Returns `None` when
    /// nothing matches (a no-match search surfaces as a GraphQL error, treated as absence).
    pub(crate) async fn fetch_metadata_by_title(
        &self,
        title: &str,
    ) -> anyhow::Result<Option<MediaMetadata>> {
        let Ok(data) = self
            .graphql_public(METADATA_QUERY, serde_json::json!({ "search": title }))
            .await
        else {
            return Ok(None);
        };
        Ok(parse_media_metadata(&data))
    }
}
