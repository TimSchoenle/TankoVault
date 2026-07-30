//! Transport and `OAuth2` for `AniList`: the HTTP client, the paced+retried GraphQL round
//! trip, and the token endpoint. The typed GraphQL operations built on top live in
//! [`super::graphql`]; the response shaping in [`super::parse`].

use std::time::Duration;

use anyhow::{Context, anyhow};
use serde::Deserialize;
use time::OffsetDateTime;

use tankovault_domain::{Pacer, PacingPolicy};

use crate::provider::OAuthTokens;

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
            // The default policy is the crawler's: +500ms on the first 429, doubling to a
            // ceiling of 8s, halving after a quiet minute.
            pacer: Pacer::new(min_interval, PacingPolicy::default()),
        })
    }

    /// The URL the user is redirected to in order to grant access.
    ///
    /// The query is built with `url`'s serializer rather than a hand-rolled percent-encoder
    /// (ARCH-7). RFC 6749 §3.1 specifies the authorization endpoint's query as
    /// `application/x-www-form-urlencoded`, which is exactly what this produces.
    #[must_use]
    pub(crate) fn authorize_url(&self) -> String {
        let query = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("client_id", &self.client_id)
            .append_pair("redirect_uri", &self.redirect_uri)
            .append_pair("response_type", "code")
            .finish();
        format!("{}/authorize?{query}", self.oauth_base)
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
        self.wait_for_slot().await;
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

    /// Execute a GraphQL operation, returning the `data` object. Retries once on `429`.
    pub(super) async fn graphql(
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
    pub(super) async fn graphql_public(
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
            self.wait_for_slot().await;
            let mut req = self.http.post(&self.graphql_url).json(&body);
            if let Some(token) = access_token {
                req = req.bearer_auth(token);
            }
            let resp = req.send().await.context("AniList GraphQL request failed")?;

            if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
                // Record the signal on *every* 429, not only the retried one: the penalty is what
                // makes the requests after this one back off too. Previously the retry slept once
                // and every later call went straight back to the base interval, so a sync run
                // against a throttling provider kept offering full rate (ARCH-20).
                let retry_after = resp
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
                    .map(Duration::from_secs);
                self.pacer.penalise(std::time::Instant::now(), retry_after);
                if attempt == 0 {
                    // The next `wait_for_slot` already carries the widened gap, so there is
                    // nothing to sleep for here.
                    continue;
                }
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

    /// Wait for this client's next paced slot.
    ///
    /// The pacer is [`tankovault_domain::Pacer`], shared with the crawler's rate limiter
    /// (ARCH-20). This client used to carry a private minimum-gap mutex with **no persistent
    /// throttle penalty**: it retried a `429` once and then went straight back to full rate,
    /// which is the behaviour a provider reads as ignoring them. The shared pacer keeps the
    /// penalty, so a throttle signal widens every later gap until `AniList` has been quiet for
    /// a recovery window.
    async fn wait_for_slot(&self) {
        let delay = self.pacer.reserve(std::time::Instant::now());
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::AniListClient;
    use std::time::Duration;

    fn client(client_id: &str, redirect_uri: &str) -> AniListClient {
        AniListClient::new(
            "https://graphql.example".to_owned(),
            "https://oauth.example".to_owned(),
            client_id.to_owned(),
            "secret".to_owned(),
            redirect_uri.to_owned(),
            Duration::from_millis(1),
        )
        .expect("client")
    }

    /// The consent URL used to be assembled with a hand-rolled percent-encoder (ARCH-7). This
    /// pins that the replacement still escapes everything that would otherwise break the query
    /// — a redirect URI's `:`, `/` and `?` above all.
    #[test]
    fn the_authorize_url_escapes_every_reserved_character() {
        let url = client("46552", "https://app.example/callback?next=/console").authorize_url();
        assert_eq!(
            url,
            "https://oauth.example/authorize?client_id=46552\
             &redirect_uri=https%3A%2F%2Fapp.example%2Fcallback%3Fnext%3D%2Fconsole\
             &response_type=code"
        );
    }

    /// The replacement encodes to `application/x-www-form-urlencoded`, which differs from the
    /// old hand-rolled RFC-3986 encoder in two places: a space becomes `+` rather than `%20`,
    /// and `~` is escaped. That is the encoding RFC 6749 §3.1 specifies for this endpoint, so
    /// the difference is the fix rather than a regression — stated here so nobody "corrects"
    /// it back.
    #[test]
    fn the_authorize_url_uses_form_encoding_not_rfc_3986() {
        let url = client("a b~c", "https://a.example/cb").authorize_url();
        assert!(
            url.contains("client_id=a+b%7Ec"),
            "expected form encoding: {url}"
        );
    }
}
