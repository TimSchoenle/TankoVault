//! `AniList` `OAuth2` + GraphQL client (design §15).
//!
//! Network I/O lives here behind small, typed methods; the response-shaping logic that is
//! easy to get wrong ([`parse_media_list`]) is a pure function with unit tests. Requests
//! are paced to stay within `AniList`'s published rate limit and retried once on `429`.

use std::fmt::Write as _;
use std::time::Duration;

use crate::mapping::{AniListStatus, content_type_from_country};
use crate::provider::{ExternalProvider, OAuthTokens, RemoteEntry, RemoteMetadata, Viewer};
use anyhow::{Context, anyhow};
use async_trait::async_trait;
use serde::Deserialize;
use tankovault_domain::{ContentType, WatchStatus};
use time::OffsetDateTime;
use tokio::sync::Mutex;
use tokio::time::Instant;
use tracing::info;

/// Default `AniList` GraphQL endpoint.
pub(crate) const DEFAULT_GRAPHQL_URL: &str = "https://graphql.anilist.co";
/// Default `AniList` OAuth base (authorize + token live under here).
pub(crate) const DEFAULT_OAUTH_BASE: &str = "https://anilist.co/api/v2/oauth";
/// The provider key used in `external_accounts` / `sync_mappings`.
pub(crate) const PROVIDER: &str = "anilist";

/// One entry from a user's `AniList` manga list, `AniList`-shaped (numeric id, `AniList`'s own
/// status vocabulary). Converted to the provider-agnostic [`RemoteEntry`] via `From` below —
/// `crate::engine::SyncEngine` only ever sees the shared type.
#[derive(Debug, Clone)]
pub(crate) struct AniListEntry {
    pub(crate) media_id: i64,
    /// Candidate titles (romaji/english/native, then every AniList synonym), non-empty
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
    /// AniList matching use the metadata adapters now capture).
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
/// from the public GraphQL API. Converted to the provider-agnostic [`RemoteMetadata`] via
/// `From` below so the enrichment worker never sees `AniList`-shaped types.
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

/// `AniList` API client. Cheap to share behind an `Arc`.
pub(crate) struct AniListClient {
    http: reqwest::Client,
    graphql_url: String,
    oauth_base: String,
    client_id: String,
    client_secret: String,
    redirect_uri: String,
    pacer: Pacer,
}

impl AniListClient {
    /// Construct a client. `min_interval` paces every outbound request (`AniList` allows
    /// ~90 requests/minute; a ~700 ms floor stays comfortably under that).
    pub(crate) fn new(
        graphql_url: String,
        oauth_base: String,
        client_id: String,
        client_secret: String,
        redirect_uri: String,
        min_interval: Duration,
    ) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent("tankovault-sync/0.1 (+https://github.com/tankovault)")
            .timeout(Duration::from_secs(30))
            .build()
            .context("building AniList HTTP client")?;
        Ok(Self {
            http,
            graphql_url,
            oauth_base,
            client_id,
            client_secret,
            redirect_uri,
            pacer: Pacer::new(min_interval),
        })
    }

    /// The URL the user is redirected to in order to grant access.
    #[must_use]
    pub(crate) fn authorize_url(&self) -> String {
        format!(
            "{}/authorize?client_id={}&redirect_uri={}&response_type=code",
            self.oauth_base,
            urlencode(&self.client_id),
            urlencode(&self.redirect_uri),
        )
    }

    /// Exchange an authorization `code` for tokens.
    pub(crate) async fn exchange_code(&self, code: &str) -> anyhow::Result<OAuthTokens> {
        let body = serde_json::json!({
            "grant_type": "authorization_code",
            "client_id": self.client_id,
            "client_secret": self.client_secret,
            "redirect_uri": self.redirect_uri,
            "code": code,
        });
        self.token_request(&body).await
    }

    /// Refresh an access token, where the provider supports it.
    pub(crate) async fn refresh(&self, refresh_token: &str) -> anyhow::Result<OAuthTokens> {
        let body = serde_json::json!({
            "grant_type": "refresh_token",
            "client_id": self.client_id,
            "client_secret": self.client_secret,
            "refresh_token": refresh_token,
        });
        self.token_request(&body).await
    }

    async fn token_request(&self, body: &serde_json::Value) -> anyhow::Result<OAuthTokens> {
        #[derive(Deserialize)]
        struct TokenResponse {
            access_token: String,
            #[serde(default)]
            refresh_token: Option<String>,
            #[serde(default)]
            expires_in: Option<i64>,
        }
        self.pacer.wait().await;
        let resp = self
            .http
            .post(format!("{}/token", self.oauth_base))
            .json(body)
            .send()
            .await
            .context("AniList token request failed")?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!("AniList token endpoint returned {status}: {text}"));
        }
        let parsed: TokenResponse =
            serde_json::from_str(&text).context("decoding AniList token response")?;
        let expires_at = parsed
            .expires_in
            .and_then(|secs| OffsetDateTime::now_utc().checked_add(time::Duration::seconds(secs)));
        Ok(OAuthTokens {
            access_token: parsed.access_token,
            refresh_token: parsed.refresh_token,
            expires_at,
        })
    }

    /// Resolve the authenticated viewer's `AniList` user id and display name (the latter is
    /// cached against the linked account so the UI can show "Connected as X").
    pub(crate) async fn viewer(&self, access_token: &str) -> anyhow::Result<Viewer> {
        const QUERY: &str = "query { Viewer { id name } }";
        let data = self
            .graphql(access_token, QUERY, serde_json::json!({}))
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
    /// `AniList` returns a user's list in *chunks* (`perChunk` is capped at 500 by the API);
    /// a large list therefore spans several responses. We page through every chunk via the
    /// `hasNextChunk` flag and concatenate the results, so the whole list is returned rather
    /// than only its first page.
    pub(crate) async fn fetch_media_list(
        &self,
        access_token: &str,
        user_id: i64,
    ) -> anyhow::Result<Vec<AniListEntry>> {
        const QUERY: &str = "\
            query ($userId: Int, $chunk: Int, $perChunk: Int) { \
              MediaListCollection(userId: $userId, type: MANGA, chunk: $chunk, perChunk: $perChunk) { \
                hasNextChunk \
                lists { entries { \
                  status progress updatedAt \
                  media { id countryOfOrigin startDate { year } \
                          title { romaji english native } \
                          synonyms \
                          genres \
                          staff(sort: RELEVANCE, perPage: 5) { edges { node { name { full } } } } } \
                } } \
              } \
            }";
        const PER_CHUNK: i64 = 500;

        let mut all = Vec::new();
        let mut chunk = 1;
        loop {
            let data = self
                .graphql(
                    access_token,
                    QUERY,
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
        access_token: &str,
        media_id: i64,
        status: AniListStatus,
        progress: i64,
    ) -> anyhow::Result<()> {
        const MUTATION: &str = "\
            mutation ($mediaId: Int, $status: MediaListStatus, $progress: Int) { \
              SaveMediaListEntry(mediaId: $mediaId, status: $status, progress: $progress) { id } \
            }";
        let vars = serde_json::json!({
            "mediaId": media_id,
            "status": status.as_graphql(),
            "progress": progress,
        });
        self.graphql(access_token, MUTATION, vars).await?;
        Ok(())
    }

    /// Best-effort search for a manga's `AniList` media id by title.
    pub(crate) async fn search_media(
        &self,
        access_token: &str,
        title: &str,
    ) -> anyhow::Result<Option<i64>> {
        const QUERY: &str =
            "query ($search: String) { Media(search: $search, type: MANGA) { id } }";
        // A no-match search yields a GraphQL error; treat that as "not found".
        let Ok(data) = self
            .graphql(access_token, QUERY, serde_json::json!({ "search": title }))
            .await
        else {
            return Ok(None);
        };
        Ok(data
            .get("Media")
            .and_then(|m| m.get("id"))
            .and_then(serde_json::Value::as_i64))
    }

    /// The full public media-metadata GraphQL fragment (no user token required). Shared by
    /// the id- and title-keyed public lookups.
    const METADATA_QUERY: &str = "\
        query ($id: Int, $search: String) { \
          Media(id: $id, search: $search, type: MANGA) { \
            id countryOfOrigin startDate { year } \
            description(asHtml: false) \
            coverImage { extraLarge large } \
            title { romaji english native } \
            synonyms \
            genres \
            staff(sort: RELEVANCE, perPage: 5) { edges { node { name { full } } } } \
          } \
        }";

    /// Fetch public metadata for a known `AniList` media id — **no user token**. Returns
    /// `None` when the id no longer resolves.
    pub(crate) async fn fetch_metadata_by_id(
        &self,
        media_id: i64,
    ) -> anyhow::Result<Option<MediaMetadata>> {
        let Ok(data) = self
            .graphql_public(Self::METADATA_QUERY, serde_json::json!({ "id": media_id }))
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
            .graphql_public(Self::METADATA_QUERY, serde_json::json!({ "search": title }))
            .await
        else {
            return Ok(None);
        };
        Ok(parse_media_metadata(&data))
    }

    /// Execute a GraphQL operation, returning the `data` object. Retries once on `429`.
    async fn graphql(
        &self,
        access_token: &str,
        query: &str,
        variables: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        self.graphql_inner(Some(access_token), query, variables)
            .await
    }

    /// Execute a GraphQL operation with **no** bearer token — used for `AniList`'s public,
    /// unauthenticated metadata endpoint (the tokenless enrichment path).
    async fn graphql_public(
        &self,
        query: &str,
        variables: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        self.graphql_inner(None, query, variables).await
    }

    async fn graphql_inner(
        &self,
        access_token: Option<&str>,
        query: &str,
        variables: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        let body = serde_json::json!({ "query": query, "variables": variables });
        for attempt in 0..2 {
            self.pacer.wait().await;
            let mut req = self.http.post(&self.graphql_url).json(&body);
            if let Some(token) = access_token {
                req = req.bearer_auth(token);
            }
            let resp = req.send().await.context("AniList GraphQL request failed")?;

            if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS && attempt == 0 {
                let retry_after = resp
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(2);
                tokio::time::sleep(Duration::from_secs(retry_after)).await;
                continue;
            }

            let status = resp.status();
            let value: serde_json::Value = resp
                .json()
                .await
                .context("decoding AniList GraphQL response")?;
            if let Some(errors) = value.get("errors").filter(|e| !e.is_null()) {
                return Err(anyhow!("AniList GraphQL error ({status}): {errors}"));
            }
            return value
                .get("data")
                .cloned()
                .ok_or_else(|| anyhow!("AniList response missing `data`"));
        }
        Err(anyhow!("AniList GraphQL rate-limited after retry"))
    }
}

/// Round a fractional local progress to the whole-chapter count `AniList` expects.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub(crate) fn progress_to_int(progress: f64) -> i64 {
    progress.max(0.0).round() as i64
}

#[async_trait]
impl ExternalProvider for AniListClient {
    fn slug(&self) -> &'static str {
        PROVIDER
    }

    fn display_name(&self) -> &'static str {
        "AniList"
    }

    fn authorize_url(&self) -> String {
        self.authorize_url()
    }

    async fn exchange_code(&self, code: &str) -> anyhow::Result<OAuthTokens> {
        self.exchange_code(code).await
    }

    async fn refresh(&self, refresh_token: &str) -> anyhow::Result<OAuthTokens> {
        self.refresh(refresh_token).await
    }

    async fn viewer(&self, access_token: &str) -> anyhow::Result<Viewer> {
        self.viewer(access_token).await
    }

    async fn fetch_list(
        &self,
        access_token: &str,
        viewer: &Viewer,
    ) -> anyhow::Result<Vec<RemoteEntry>> {
        let user_id: i64 = viewer
            .id
            .parse()
            .context("AniList viewer id was not numeric")?;
        Ok(self
            .fetch_media_list(access_token, user_id)
            .await?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    async fn search(&self, access_token: &str, title: &str) -> anyhow::Result<Option<String>> {
        Ok(self
            .search_media(access_token, title)
            .await?
            .map(|id| id.to_string()))
    }

    fn supports_public_metadata(&self) -> bool {
        true
    }

    async fn fetch_public_metadata_by_title(
        &self,
        title: &str,
    ) -> anyhow::Result<Option<RemoteMetadata>> {
        Ok(self.fetch_metadata_by_title(title).await?.map(Into::into))
    }

    async fn fetch_public_metadata_by_id(
        &self,
        external_id: &str,
    ) -> anyhow::Result<Option<RemoteMetadata>> {
        let media_id: i64 = external_id
            .parse()
            .context("AniList external id was not numeric")?;
        Ok(self.fetch_metadata_by_id(media_id).await?.map(Into::into))
    }

    async fn save_entry(
        &self,
        access_token: &str,
        external_id: &str,
        status: WatchStatus,
        progress: f64,
    ) -> anyhow::Result<()> {
        let media_id: i64 = external_id
            .parse()
            .context("AniList external id was not numeric")?;
        self.save_entry(
            access_token,
            media_id,
            AniListStatus::from_watch_status(status),
            progress_to_int(progress),
        )
        .await
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
    let start_year = media
        .get("startDate")
        .and_then(|d| d.get("year"))
        .and_then(serde_json::Value::as_i64)
        .and_then(|y| i32::try_from(y).ok());
    let content_type = content_type_from_country(
        media
            .get("countryOfOrigin")
            .and_then(serde_json::Value::as_str),
    );

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
    let start_year = media
        .get("startDate")
        .and_then(|d| d.get("year"))
        .and_then(serde_json::Value::as_i64)
        .and_then(|y| i32::try_from(y).ok());
    let content_type = content_type_from_country(
        media
            .get("countryOfOrigin")
            .and_then(serde_json::Value::as_str),
    );
    Some(MediaMetadata {
        media_id,
        titles,
        description,
        cover_url,
        start_year,
        content_type,
        tags: genres_from_media(media),
        authors: staff_from_media(media),
    })
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

/// Minimal per-client request pacer: enforces a minimum gap between outbound calls.
struct Pacer {
    min_interval: Duration,
    last: Mutex<Option<Instant>>,
}

impl Pacer {
    fn new(min_interval: Duration) -> Self {
        Self {
            min_interval,
            last: Mutex::new(None),
        }
    }

    async fn wait(&self) {
        if self.min_interval.is_zero() {
            return;
        }
        let mut guard = self.last.lock().await;
        let now = Instant::now();
        if let Some(prev) = *guard {
            let elapsed = now.duration_since(prev);
            if elapsed < self.min_interval {
                tokio::time::sleep(self.min_interval.checked_sub(elapsed).unwrap()).await;
            }
        }
        *guard = Some(Instant::now());
    }
}

/// Percent-encode a query-string component (RFC 3986 unreserved set preserved).
fn urlencode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            other => {
                let _ = write!(out, "%{other:02X}");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    // Tests assert exact equality of small, exactly-representable progress/id values.
    #![allow(clippy::float_cmp)]

    use super::*;

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
    fn urlencode_escapes_reserved_characters() {
        assert_eq!(urlencode("a b/c?d"), "a%20b%2Fc%3Fd");
        assert_eq!(urlencode("safe-_.~"), "safe-_.~");
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
