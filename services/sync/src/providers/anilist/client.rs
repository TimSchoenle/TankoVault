//! Transport and `OAuth2` for `AniList`: the HTTP client, the paced+retried GraphQL round
//! trip, and the token endpoint. The typed GraphQL operations built on top live in
//! [`super::graphql`]; the response shaping in [`super::parse`].

use std::time::Duration;

use anyhow::{Context, anyhow};
use secrecy::{ExposeSecret as _, SecretString};
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
    client_secret: SecretString,
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
        client_secret: SecretString,
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

    /// The URL the user is redirected to in order to grant access. Built with `url`'s
    /// serializer, producing the `application/x-www-form-urlencoded` query RFC 6749 §3.1 requires.
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
            "client_secret": self.client_secret.expose_secret(),
            "redirect_uri": self.redirect_uri,
            "code": code,
        });
        self.token_request(&body).await
    }

    /// Refresh an access token, where the provider supports it.
    pub(crate) async fn refresh(
        &self,
        refresh_token: &SecretString,
    ) -> anyhow::Result<OAuthTokens> {
        let body = serde_json::json!({
            "grant_type": "refresh_token",
            "client_id": self.client_id,
            "client_secret": self.client_secret.expose_secret(),
            "refresh_token": refresh_token.expose_secret(),
        });
        self.token_request(&body).await
    }

    async fn token_request(&self, body: &serde_json::Value) -> anyhow::Result<OAuthTokens> {
        #[derive(Deserialize)]
        struct TokenResponse {
            // `SecretString` even inside this throwaway decode struct: the response body is
            // already in `text` below, and the point is that the *parsed* tokens cannot be
            // logged or `dbg!`-ed on their way into `OAuthTokens`.
            access_token: SecretString,
            #[serde(default)]
            refresh_token: Option<SecretString>,
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
        access_token: &SecretString,
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
        access_token: Option<&SecretString>,
        query: &str,
        variables: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        let body = serde_json::json!({ "query": query, "variables": variables });
        for attempt in 0..2 {
            self.wait_for_slot().await;
            let mut req = self.http.post(&self.graphql_url).json(&body);
            if let Some(token) = access_token {
                // The one place a stored `AniList` token is unwrapped: into the `Authorization`
                // header of the request it authenticates.
                req = req.bearer_auth(token.expose_secret());
            }
            let resp = req.send().await.context("AniList GraphQL request failed")?;

            if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
                // Recorded on every 429, not only the retried one, so later requests back off
                // too instead of reverting to full rate after one sleep.
                let retry_after = resp
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
                    .map(Duration::from_secs);
                self.pacer.penalise(std::time::Instant::now(), retry_after);
                if attempt == 0 {
                    // Must sleep the penalty out here, explicitly: `Pacer::reserve` starts a
                    // fresh schedule once its last-handed-out slot has elapsed (always true by
                    // the time a response returns), so `continue`-ing straight into
                    // `wait_for_slot` would retry with no delay at all and ignore `Retry-After`.
                    let penalty = self.pacer.penalty(std::time::Instant::now());
                    if !penalty.is_zero() {
                        tokio::time::sleep(penalty).await;
                    }
                    continue;
                }
                // Break rather than fall through to the decoder: persistent throttling used to
                // surface as a JSON parse error, hiding the actual rate-limit cause.
                break;
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

    /// Wait for this client's next paced slot. Uses the shared [`tankovault_domain::Pacer`],
    /// which keeps a persistent throttle penalty rather than reverting to full rate after one
    /// retry, so a `429` widens every later gap until `AniList` has been quiet for a while.
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
    use secrecy::{ExposeSecret, SecretString};
    use std::time::{Duration, Instant};
    use time::OffsetDateTime;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client(client_id: &str, redirect_uri: &str) -> AniListClient {
        AniListClient::new(
            "https://graphql.example".to_owned(),
            "https://oauth.example".to_owned(),
            client_id.to_owned(),
            SecretString::from("secret"),
            redirect_uri.to_owned(),
            Duration::from_millis(1),
        )
        .expect("client")
    }

    /// A client pointed at a scripted upstream; `min_interval` is kept at 1 ms so a timing
    /// assertion below measures only the penalty.
    fn client_for(server: &MockServer, min_interval: Duration) -> AniListClient {
        AniListClient::new(
            format!("{}/graphql", server.uri()),
            server.uri(),
            "46552".to_owned(),
            SecretString::from("secret"),
            "https://app.example/cb".to_owned(),
            min_interval,
        )
        .expect("client")
    }

    /// The JSON body of the one request the server received.
    async fn sole_request_body(server: &MockServer) -> serde_json::Value {
        let requests = server.received_requests().await.expect("request recording");
        assert_eq!(requests.len(), 1, "expected exactly one request");
        serde_json::from_slice(&requests[0].body).expect("a JSON request body")
    }

    /// Mount a `POST /graphql` that answers `200` with `data` for every call.
    async fn mount_graphql_ok(server: &MockServer) {
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "data": { "ok": 1 } })),
            )
            .mount(server)
            .await;
    }

    /// Pins that reserved characters in the redirect URI (`:`, `/`, `?`) are escaped in the
    /// consent URL query.
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

    /// Pins `application/x-www-form-urlencoded` encoding (space as `+`, `~` escaped) rather than
    /// RFC-3986 — the encoding RFC 6749 §3.1 specifies, not a regression to "fix" back.
    #[test]
    fn the_authorize_url_uses_form_encoding_not_rfc_3986() {
        let url = client("a b~c", "https://a.example/cb").authorize_url();
        assert!(
            url.contains("client_id=a+b%7Ec"),
            "expected form encoding: {url}"
        );
    }

    /// Asserts the exact grant body (all five RFC 6749 §4.1.3 members): checking only some
    /// fields would pass even with `client_secret` silently dropped.
    #[tokio::test]
    async fn the_code_exchange_posts_the_authorization_code_grant_and_returns_the_tokens() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "at-1",
                "refresh_token": "rt-1",
                "expires_in": 3600,
            })))
            .expect(1)
            .mount(&server)
            .await;

        let before = OffsetDateTime::now_utc();
        let tokens = client_for(&server, Duration::from_millis(1))
            .exchange_code("the-code")
            .await
            .expect("the exchange succeeds");
        let after = OffsetDateTime::now_utc();

        // `SecretString` has no `PartialEq`; exposed here deliberately since these are fixtures.
        assert_eq!(tokens.access_token.expose_secret(), "at-1");
        assert_eq!(
            tokens
                .refresh_token
                .as_ref()
                .map(ExposeSecret::expose_secret),
            Some("rt-1")
        );
        let expires_at = tokens.expires_at.expect("expires_in yields an expiry");
        assert!(
            expires_at >= before + time::Duration::seconds(3600)
                && expires_at <= after + time::Duration::seconds(3600),
            "expiry {expires_at} is not 3600s after the request"
        );

        assert_eq!(
            sole_request_body(&server).await,
            serde_json::json!({
                "grant_type": "authorization_code",
                "client_id": "46552",
                "client_secret": "secret",
                "redirect_uri": "https://app.example/cb",
                "code": "the-code",
            })
        );
    }

    /// RFC 6749 §6's refresh grant has no `redirect_uri`/`code`, and a response lacking
    /// `refresh_token`/`expires_in` (both `#[serde(default)]`) must still be a success.
    #[tokio::test]
    async fn a_refresh_sends_the_refresh_grant_and_accepts_a_response_carrying_only_a_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "access_token": "at-2" })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let tokens = client_for(&server, Duration::from_millis(1))
            .refresh(&SecretString::from("rt-old"))
            .await
            .expect("the refresh succeeds");

        assert_eq!(tokens.access_token.expose_secret(), "at-2");
        assert!(tokens.refresh_token.is_none());
        assert_eq!(tokens.expires_at, None);

        assert_eq!(
            sole_request_body(&server).await,
            serde_json::json!({
                "grant_type": "refresh_token",
                "client_id": "46552",
                "client_secret": "secret",
                "refresh_token": "rt-old",
            })
        );
    }

    /// `AniList` answers `400` alike for a spent code, wrong secret, or mismatched redirect,
    /// distinguished only in the body — so both status and body must be asserted.
    #[tokio::test]
    async fn a_rejected_token_request_reports_the_status_and_the_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(
                serde_json::json!({ "error": "invalid_grant", "hint": "code expired" }),
            ))
            .expect(1)
            .mount(&server)
            .await;

        let err = client_for(&server, Duration::from_millis(1))
            .exchange_code("stale")
            .await
            .expect_err("a 400 is an error");
        let message = err.to_string();
        assert!(message.contains("400"), "no status in: {message}");
        assert!(message.contains("invalid_grant"), "no body in: {message}");
    }

    /// Both legs asserted together: the failure worth catching is `graphql_public` acquiring a
    /// bearer token, attributing catalogue enrichment to one user's rate-limit budget.
    #[tokio::test]
    async fn the_authenticated_call_carries_a_bearer_token_and_the_public_one_carries_none() {
        let server = MockServer::start().await;
        mount_graphql_ok(&server).await;

        let client = client_for(&server, Duration::from_millis(1));
        client
            .graphql(
                &SecretString::from("tok-1"),
                "query {}",
                serde_json::json!({}),
            )
            .await
            .expect("authenticated call");
        client
            .graphql_public("query {}", serde_json::json!({}))
            .await
            .expect("public call");

        let requests = server.received_requests().await.expect("request recording");
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests[0]
                .headers
                .get("authorization")
                .map(|v| v.to_str().expect("ASCII header")),
            Some("Bearer tok-1")
        );
        assert!(
            requests[1].headers.get("authorization").is_none(),
            "the public path sent a credential"
        );
    }

    /// Asserts the request *count* too: returning the first (data-less) response as-is, or
    /// retrying twice, would both otherwise look like a pass.
    #[tokio::test]
    async fn a_429_is_retried_once_and_the_retry_is_what_answers() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .respond_with(ResponseTemplate::new(429))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        mount_graphql_ok(&server).await;

        let data = client_for(&server, Duration::from_millis(1))
            .graphql_public("query {}", serde_json::json!({}))
            .await
            .expect("the retry succeeds");

        assert_eq!(data, serde_json::json!({ "ok": 1 }));
        assert_eq!(
            server
                .received_requests()
                .await
                .expect("request recording")
                .len(),
            2
        );
    }

    /// Pins the bug where a second `429` fell through to the JSON decoder instead of the
    /// retry-exhausted error, reporting persistent throttling as a decode failure.
    #[tokio::test]
    async fn a_second_429_is_reported_as_rate_limiting_rather_than_as_a_decode_failure() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .respond_with(ResponseTemplate::new(429))
            .expect(2)
            .mount(&server)
            .await;

        let err = client_for(&server, Duration::from_millis(1))
            .graphql_public("query {}", serde_json::json!({}))
            .await
            .expect_err("two 429s exhaust the retry");
        let message = err.to_string();
        assert!(
            message.contains("rate-limited after retry"),
            "not reported as rate limiting: {message}"
        );
        assert!(
            !message.contains("decoding"),
            "still reported as a decode failure: {message}"
        );
    }

    /// Pins that a `429` widens the gap for every later request, not just the immediate retry —
    /// the old minimum-gap mutex reverted to full rate right after. Measured on a separate later
    /// call, not the retry itself, since the retry is slow under either implementation.
    #[tokio::test]
    async fn a_429_widens_the_gap_for_later_requests_not_only_for_the_retry() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .respond_with(ResponseTemplate::new(429))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        mount_graphql_ok(&server).await;

        let client = client_for(&server, Duration::from_millis(1));
        client
            .graphql_public("query {}", serde_json::json!({}))
            .await
            .expect("the retry succeeds");

        let started = Instant::now();
        client
            .graphql_public("query {}", serde_json::json!({}))
            .await
            .expect("the later call succeeds");
        assert!(
            started.elapsed() >= Duration::from_millis(400),
            "the penalty did not outlive the retry: {:?}",
            started.elapsed()
        );
    }

    /// The inverse leg: without it, a client that always slept half a second would satisfy the
    /// lower-bound assertion above while pacing nothing.
    #[tokio::test]
    async fn an_unthrottled_client_pays_no_penalty_gap() {
        let server = MockServer::start().await;
        mount_graphql_ok(&server).await;

        let client = client_for(&server, Duration::from_millis(1));
        client
            .graphql_public("query {}", serde_json::json!({}))
            .await
            .expect("first call");

        let started = Instant::now();
        client
            .graphql_public("query {}", serde_json::json!({}))
            .await
            .expect("second call");
        assert!(
            started.elapsed() < Duration::from_millis(300),
            "an unthrottled client waited like a throttled one: {:?}",
            started.elapsed()
        );
    }

    /// `Retry-After` parsing is this module's own code and needs its own test: dropping it
    /// falls back to the 500 ms default step, well short of the 2 s asserted here.
    #[tokio::test]
    async fn a_numeric_retry_after_is_honoured_as_the_floor_for_the_gap() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "2"))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        mount_graphql_ok(&server).await;

        let started = Instant::now();
        client_for(&server, Duration::from_millis(1))
            .graphql_public("query {}", serde_json::json!({}))
            .await
            .expect("the retry succeeds");
        assert!(
            started.elapsed() >= Duration::from_millis(1500),
            "Retry-After was ignored: {:?}",
            started.elapsed()
        );
    }

    /// GraphQL reports application errors with `200 OK` and an `errors` array, so the status line
    /// is not the answer. An expired token arrives this way.
    #[tokio::test]
    async fn a_200_carrying_a_graphql_errors_array_is_an_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": null,
                "errors": [{ "message": "Invalid token" }],
            })))
            .mount(&server)
            .await;

        let err = client_for(&server, Duration::from_millis(1))
            .graphql(
                &SecretString::from("stale"),
                "query {}",
                serde_json::json!({}),
            )
            .await
            .expect_err("an errors array is an error");
        assert!(
            err.to_string().contains("Invalid token"),
            "the provider's message was dropped: {err}"
        );
    }

    /// `AniList` sends `"errors": null` on good responses, so the check is
    /// `filter(|e| !e.is_null())`, not a bare `get("errors")` that would reject them all.
    #[tokio::test]
    async fn a_null_errors_key_beside_real_data_is_a_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": { "ok": 1 },
                "errors": null,
            })))
            .mount(&server)
            .await;

        let data = client_for(&server, Duration::from_millis(1))
            .graphql_public("query {}", serde_json::json!({}))
            .await
            .expect("a null errors key is not an error");
        assert_eq!(data, serde_json::json!({ "ok": 1 }));
    }

    /// A `200` with neither `data` nor `errors` is malformed rather than empty, and saying so is
    /// what keeps the caller from treating a missing object as "no results".
    #[tokio::test]
    async fn a_response_without_data_is_an_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&server)
            .await;

        let err = client_for(&server, Duration::from_millis(1))
            .graphql_public("query {}", serde_json::json!({}))
            .await
            .expect_err("a response without data is an error");
        assert!(
            err.to_string().contains("missing `data`"),
            "unexpected message: {err}"
        );
    }
}
