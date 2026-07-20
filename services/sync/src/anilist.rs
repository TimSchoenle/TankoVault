//! `AniList` `OAuth2` + GraphQL client (design §15).
//!
//! Network I/O lives here behind small, typed methods; the response-shaping logic that is
//! easy to get wrong ([`parse_media_list`]) is a pure function with unit tests. Requests
//! are paced to stay within `AniList`'s published rate limit and retried once on `429`.

use std::fmt::Write as _;
use std::time::Duration;

use anyhow::{Context, anyhow};
use serde::Deserialize;
use time::OffsetDateTime;
use tokio::sync::Mutex;
use tokio::time::Instant;

use crate::mapping::{AniListStatus, content_type_from_country};
use tankovault_domain::ContentType;

/// Default `AniList` GraphQL endpoint.
pub(crate) const DEFAULT_GRAPHQL_URL: &str = "https://graphql.anilist.co";
/// Default `AniList` OAuth base (authorize + token live under here).
pub(crate) const DEFAULT_OAUTH_BASE: &str = "https://anilist.co/api/v2/oauth";
/// The provider key used in `external_accounts` / `sync_mappings`.
pub(crate) const PROVIDER: &str = "anilist";

/// OAuth tokens returned by the `AniList` token endpoint.
#[derive(Debug, Clone)]
pub(crate) struct OAuthTokens {
    pub(crate) access_token: String,
    pub(crate) refresh_token: Option<String>,
    pub(crate) expires_at: Option<OffsetDateTime>,
}

/// One entry from a user's `AniList` manga list, normalised for local matching.
#[derive(Debug, Clone)]
pub(crate) struct RemoteEntry {
    pub(crate) media_id: i64,
    /// Candidate titles (romaji/english/native), non-empty ones only.
    pub(crate) titles: Vec<String>,
    pub(crate) status: AniListStatus,
    pub(crate) progress: f64,
    pub(crate) updated_at: OffsetDateTime,
    pub(crate) start_year: Option<i32>,
    pub(crate) content_type: ContentType,
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

    /// Resolve the authenticated viewer's `AniList` user id.
    pub(crate) async fn viewer_id(&self, access_token: &str) -> anyhow::Result<i64> {
        const QUERY: &str = "query { Viewer { id } }";
        let data = self
            .graphql(access_token, QUERY, serde_json::json!({}))
            .await?;
        data.get("Viewer")
            .and_then(|v| v.get("id"))
            .and_then(serde_json::Value::as_i64)
            .ok_or_else(|| anyhow!("AniList Viewer query returned no id"))
    }

    /// Fetch the viewer's full manga list.
    pub(crate) async fn fetch_media_list(
        &self,
        access_token: &str,
        user_id: i64,
    ) -> anyhow::Result<Vec<RemoteEntry>> {
        const QUERY: &str = "\
            query ($userId: Int) { \
              MediaListCollection(userId: $userId, type: MANGA) { \
                lists { entries { \
                  status progress updatedAt \
                  media { id countryOfOrigin startDate { year } \
                          title { romaji english native } } \
                } } \
              } \
            }";
        let data = self
            .graphql(
                access_token,
                QUERY,
                serde_json::json!({ "userId": user_id }),
            )
            .await?;
        Ok(parse_media_list(&data))
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

    /// Execute a GraphQL operation, returning the `data` object. Retries once on `429`.
    async fn graphql(
        &self,
        access_token: &str,
        query: &str,
        variables: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        let body = serde_json::json!({ "query": query, "variables": variables });
        for attempt in 0..2 {
            self.pacer.wait().await;
            let resp = self
                .http
                .post(&self.graphql_url)
                .bearer_auth(access_token)
                .json(&body)
                .send()
                .await
                .context("AniList GraphQL request failed")?;

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

/// Extract [`RemoteEntry`]s from a `MediaListCollection` GraphQL `data` object. Entries
/// with an unrecognised status or no usable title are skipped rather than failing the run.
#[must_use]
pub(crate) fn parse_media_list(data: &serde_json::Value) -> Vec<RemoteEntry> {
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

fn parse_entry(entry: &serde_json::Value) -> Option<RemoteEntry> {
    let media = entry.get("media")?;
    let media_id = media.get("id").and_then(serde_json::Value::as_i64)?;
    let status = AniListStatus::parse(entry.get("status").and_then(serde_json::Value::as_str)?)?;

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

    Some(RemoteEntry {
        media_id,
        titles,
        status,
        progress,
        updated_at,
        start_year,
        content_type,
    })
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
                                       "english": "Solo Leveling", "native": null }
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
        assert_eq!(solo.titles, vec!["Na Honjaman Level Up", "Solo Leveling"]);

        let berserk = &entries[1];
        assert_eq!(berserk.content_type, ContentType::Manga);
        assert_eq!(berserk.start_year, None);
        assert_eq!(berserk.titles, vec!["Berserk", "ベルセルク"]);
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
    fn urlencode_escapes_reserved_characters() {
        assert_eq!(urlencode("a b/c?d"), "a%20b%2Fc%3Fd");
        assert_eq!(urlencode("safe-_.~"), "safe-_.~");
    }
}
