//! The innermost fetcher: `reqwest` with a validating (SSRF-safe) DNS resolver, per-hop
//! redirect scheme validation, realistic default headers, and a bounded text body.

use crate::error::FetchError;
use crate::fetcher::Fetcher;
use crate::ssrf::{self, SsrfResolver};
use crate::types::{FetchRequest, FetchResponse};
use async_trait::async_trait;
use futures::StreamExt;
use std::sync::Arc;
use std::time::Duration;
use url::Url;

/// Maximum redirects followed before erroring.
const MAX_REDIRECTS: usize = 5;
/// Hard cap on response body size (guards memory and the no-binary-content invariant).
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

/// `reqwest`-backed base fetcher. One per provider (carries that provider's default UA).
pub struct BaseHttpFetcher {
    client: reqwest::Client,
    default_user_agent: String,
}

impl BaseHttpFetcher {
    /// Build a base fetcher with the given default user-agent and timeouts.
    ///
    /// # Errors
    /// Returns [`FetchError::Transport`] if the underlying client cannot be built.
    pub fn new(
        default_user_agent: impl Into<String>,
        connect_timeout: Duration,
        request_timeout: Duration,
    ) -> Result<Self, FetchError> {
        let redirect = reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS {
                return attempt.error("too many redirects");
            }
            match attempt.url().scheme() {
                "http" | "https" => attempt.follow(),
                _ => attempt.stop(),
            }
        });

        let client = reqwest::Client::builder()
            .dns_resolver(Arc::new(SsrfResolver))
            .redirect(redirect)
            .connect_timeout(connect_timeout)
            .timeout(request_timeout)
            .build()
            .map_err(|e| FetchError::Transport(e.to_string()))?;

        Ok(Self {
            client,
            default_user_agent: default_user_agent.into(),
        })
    }
}

#[async_trait]
impl Fetcher for BaseHttpFetcher {
    async fn get(&self, req: FetchRequest) -> Result<FetchResponse, FetchError> {
        let url = Url::parse(&req.url).map_err(|_| FetchError::InvalidUrl(req.url.clone()))?;
        // Cheap pre-flight; the resolver enforces the address-range check at connect time.
        ssrf::validate_url(&url)?;

        let ua = req
            .user_agent
            .as_deref()
            .unwrap_or(&self.default_user_agent);
        let mut builder = self
            .client
            .get(url)
            .header(reqwest::header::USER_AGENT, ua)
            .header(
                reqwest::header::ACCEPT,
                "text/html,application/xhtml+xml,application/xml;q=0.9,application/json;q=0.8,*/*;q=0.5",
            )
            .header(reqwest::header::ACCEPT_LANGUAGE, "en-US,en;q=0.8");
        for (k, v) in &req.headers {
            builder = builder.header(k, v);
        }

        let resp = builder.send().await.map_err(map_reqwest_err)?;

        let status = resp.status().as_u16();
        let final_url = resp.url().to_string();
        let headers = resp
            .headers()
            .iter()
            .map(|(k, v)| {
                (
                    k.as_str().to_owned(),
                    v.to_str().unwrap_or_default().to_owned(),
                )
            })
            .collect();

        // Stream the body with a hard byte cap.
        let mut stream = resp.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(map_reqwest_err)?;
            if buf.len() + chunk.len() > MAX_BODY_BYTES {
                return Err(FetchError::BodyTooLarge);
            }
            buf.extend_from_slice(&chunk);
        }
        let body = String::from_utf8_lossy(&buf).into_owned();

        Ok(FetchResponse {
            status,
            url: final_url,
            headers,
            body,
            from_cache: false,
        })
    }
}

#[allow(clippy::needless_pass_by_value)] // owned-error mapper, used directly in `.map_err`
fn map_reqwest_err(e: reqwest::Error) -> FetchError {
    if e.is_timeout() {
        FetchError::Timeout
    } else if e.is_redirect() {
        FetchError::TooManyRedirects
    } else {
        FetchError::Transport(e.to_string())
    }
}
