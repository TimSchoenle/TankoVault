//! The innermost fetcher: `wreq` with a browser emulation profile, a validating
//! (SSRF-safe) DNS resolver, per-hop redirect scheme validation, and a bounded text body.
//!
//! Emulation exists because providers fingerprint the TLS `ClientHello` and HTTP/2 frames and
//! cross-check them against `User-Agent` — a browser UA over a non-browser handshake is a
//! *stronger* bot signal than no disguise. `wreq`'s profile owns the matching header set, so
//! this layer must never hand-set a header a profile owns; the only exception is the
//! user-agent of a solved challenge session, whose clearance cookies are bound to it.

use crate::error::FetchError;
use crate::fetcher::Fetcher;
use crate::ssrf::{self, SsrfResolver};
use crate::types::{FetchRequest, FetchResponse};
use async_trait::async_trait;
use futures::StreamExt;
use std::sync::Arc;
use std::time::Duration;
use tankovault_domain::BrowserEmulation;
use url::Url;
use wreq_util::{Emulation, Platform, Profile};

/// Maximum redirects followed before erroring.
const MAX_REDIRECTS: usize = 5;
/// Hard cap on response body size (guards memory and the no-binary-content invariant).
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

/// Resolve a domain-level browser family to a concrete emulation profile.
///
/// The newest build in each family is chosen deliberately: fingerprints of long-superseded
/// releases are themselves anomalous. Bumping `wreq-util` moves every provider forward
/// without a database migration — which is why [`BrowserEmulation`] names families, not
/// versions.
fn profile_for(browser: BrowserEmulation) -> Emulation {
    let (profile, platform) = match browser {
        BrowserEmulation::Chrome => (Profile::Chrome149, Platform::Windows),
        BrowserEmulation::Firefox => (Profile::Firefox151, Platform::Windows),
        BrowserEmulation::Safari => (Profile::Safari26_4, Platform::MacOS),
        BrowserEmulation::Edge => (Profile::Edge148, Platform::Windows),
        BrowserEmulation::OkHttp => (Profile::OkHttp5, Platform::Android),
    };
    Emulation::builder()
        .profile(profile)
        .platform(platform)
        .build()
}

/// `wreq`-backed base fetcher. One per provider (carries that provider's identity).
pub struct BaseHttpFetcher {
    client: wreq::Client,
    /// The identifiable bot user-agent, present **iff** emulation is off. `None` therefore
    /// means "a profile owns the identity headers", which is also what gates the default
    /// `Accept`/`Accept-Language` below — one field, so the two can never disagree.
    bot_user_agent: Option<String>,
}

impl BaseHttpFetcher {
    /// Build a base fetcher.
    ///
    /// `emulation` selects the browser to impersonate; `None` crawls as an identifiable bot
    /// sending `default_user_agent` verbatim. `default_user_agent` is ignored whenever
    /// `emulation` is `Some`, because the profile's user-agent is the one that matches the
    /// handshake.
    ///
    /// # Errors
    /// Returns [`FetchError::Transport`] if the underlying client cannot be built.
    pub fn new(
        default_user_agent: impl Into<String>,
        emulation: Option<BrowserEmulation>,
        connect_timeout: Duration,
        request_timeout: Duration,
    ) -> Result<Self, FetchError> {
        let redirect = wreq::redirect::Policy::custom(|attempt| {
            if attempt.previous.len() >= MAX_REDIRECTS {
                return attempt.error("too many redirects");
            }
            // `wreq` 6 carries a `http::Uri` here rather than a `url::Url`, and `Uri::scheme`
            // is `Option<&Scheme>` — a scheme-relative or malformed target yields `None`, which
            // must fall to `stop()` with the other non-HTTP schemes rather than being unwrapped.
            // Resolved before the branch because `follow`/`stop` consume `attempt`.
            let http_scheme = matches!(attempt.uri.scheme_str(), Some("http" | "https"));
            if http_scheme {
                attempt.follow()
            } else {
                attempt.stop()
            }
        });

        let mut builder = wreq::Client::builder()
            .dns_resolver(Arc::new(SsrfResolver))
            .redirect(redirect)
            .connect_timeout(connect_timeout)
            .timeout(request_timeout);
        if let Some(browser) = emulation {
            builder = builder.emulation(profile_for(browser));
        }

        let client = builder
            .build()
            .map_err(|e| FetchError::Transport(e.to_string()))?;

        Ok(Self {
            client,
            bot_user_agent: emulation.is_none().then(|| default_user_agent.into()),
        })
    }
}

#[async_trait]
impl Fetcher for BaseHttpFetcher {
    /// Times and counts the request around [`Self::send`].
    ///
    /// This is the workspace's only choke point for provider traffic — every adapter, every
    /// scan tier and both solver paths come through here — so it is where "is this provider
    /// still answering us" becomes a number instead of a log line.
    async fn get(&self, req: FetchRequest) -> Result<FetchResponse, FetchError> {
        let provider = req.provider_slug.clone();
        let started = std::time::Instant::now();
        let result = self.send(req).await;

        let outcome = match &result {
            Ok(resp) => status_class(resp.status),
            Err(_) => "error",
        };
        metrics::counter!(
            "provider_fetch_total",
            "provider" => provider.clone(),
            "outcome" => outcome,
        )
        .increment(1);
        let elapsed = started.elapsed();
        metrics::histogram!("provider_fetch_duration_seconds", "provider" => provider)
            .record(elapsed.as_secs_f64());
        // Counted even when the request failed: a scan task that spent four minutes on requests
        // that all timed out has spent four minutes, and a breakdown that omitted them would
        // report the time as unaccounted for.
        crate::accounting::record(crate::accounting::Metered::Request(elapsed));

        result
    }
}

impl BaseHttpFetcher {
    async fn send(&self, req: FetchRequest) -> Result<FetchResponse, FetchError> {
        let url = Url::parse(&req.url).map_err(|_| FetchError::InvalidUrl(req.url.clone()))?;
        // Cheap pre-flight; the resolver enforces the address-range check at connect time.
        ssrf::validate_url(&url)?;

        // `.as_str()`: `wreq` 6 takes `IntoUri` (a `http::Uri`), which `url::Url` does not
        // implement. Going through the string is the lossless hop — `Url` has already
        // normalised and, above, been SSRF-validated in that form.
        let mut builder = self.client.get(url.as_str());
        // Precedence: a solved session's user-agent, else the bot user-agent when not
        // emulating, else nothing — leaving the profile's own header untouched.
        if let Some(ua) = req.user_agent.as_deref().or(self.bot_user_agent.as_deref()) {
            builder = builder.header(wreq::header::USER_AGENT, ua);
        }
        if self.bot_user_agent.is_some() {
            // Not emulating: no profile owns the content-negotiation headers, so supply
            // plausible defaults. Under emulation these come from the profile and must not
            // be overwritten.
            builder = builder
                .header(
                    wreq::header::ACCEPT,
                    "text/html,application/xhtml+xml,application/xml;q=0.9,application/json;q=0.8,*/*;q=0.5",
                )
                .header(wreq::header::ACCEPT_LANGUAGE, "en-US,en;q=0.8");
        }
        for (k, v) in &req.headers {
            builder = builder.header(k, v);
        }

        let resp = builder.send().await.map_err(map_wreq_err)?;

        let status = resp.status().as_u16();
        let final_url = resp.uri().to_string();
        // Every header is materialised, deliberately not an allowlist of the few call sites
        // happen to read: `FetchResponse::headers` is documented as "response headers", and
        // narrowing it would make that a silent lie for whatever a future reader adds.
        let mut headers = Vec::with_capacity(resp.headers().len());
        headers.extend(resp.headers().iter().map(|(k, v)| {
            (
                k.as_str().to_owned(),
                v.to_str().unwrap_or_default().to_owned(),
            )
        }));

        // Pre-size from `Content-Length` when declared, clamped to the cap so a hostile header
        // cannot force an 8 MiB allocation for a 200-byte body.
        //
        // The header is a hint, never a bound: the streaming check below is what actually
        // enforces `MAX_BODY_BYTES`, since a server is free to send more than it announced.
        let declared = resp
            .content_length()
            .and_then(|n| usize::try_from(n).ok())
            .map_or(0, |n| n.min(MAX_BODY_BYTES));

        // Stream the body with a hard byte cap.
        let mut stream = resp.bytes_stream();
        let mut buf: Vec<u8> = Vec::with_capacity(declared);
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(map_wreq_err)?;
            if buf.len() + chunk.len() > MAX_BODY_BYTES {
                return Err(FetchError::BodyTooLarge);
            }
            buf.extend_from_slice(&chunk);
        }
        // `from_utf8` first so the happy path *moves* the buffer instead of copying up to
        // 8 MiB; `from_utf8_lossy` handles the rare body with a bad byte.
        let body = String::from_utf8(buf)
            .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());

        Ok(FetchResponse {
            status,
            url: final_url,
            headers,
            body,
            from_cache: false,
        })
    }
}

/// Fold a status into one of five label values.
///
/// The class, not the code: `provider_fetch_total` is already labelled by provider, and a
/// provider crossed with every status a hostile or broken site can return is an unbounded
/// label source for a distinction no panel makes.
fn status_class(status: u16) -> &'static str {
    match status {
        200..=299 => "2xx",
        300..=399 => "3xx",
        400..=499 => "4xx",
        500..=599 => "5xx",
        _ => "other",
    }
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "an owned-error mapper, passed directly to `.map_err`"
)]
fn map_wreq_err(e: wreq::Error) -> FetchError {
    if e.is_timeout() {
        FetchError::Timeout
    } else if e.is_redirect() {
        FetchError::TooManyRedirects
    } else {
        FetchError::Transport(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(emulation: Option<BrowserEmulation>) -> BaseHttpFetcher {
        BaseHttpFetcher::new(
            "TankoVaultBot/0.1",
            emulation,
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .expect("client builds")
    }

    /// The profile owns `User-Agent`; overriding it with the configured bot string would
    /// contradict the TLS fingerprint, so the field must be dropped when emulating.
    #[test]
    fn emulated_client_defers_to_the_profile_user_agent() {
        assert!(
            build(Some(BrowserEmulation::Chrome))
                .bot_user_agent
                .is_none()
        );
    }

    #[test]
    fn unemulated_client_sends_the_configured_bot_user_agent() {
        assert_eq!(
            build(None).bot_user_agent.as_deref(),
            Some("TankoVaultBot/0.1")
        );
    }

    /// A per-request user-agent (a solved challenge session) must still win: the clearance
    /// cookie is bound to the user-agent the solver's browser presented.
    #[test]
    fn session_user_agent_takes_precedence_over_both() {
        let f = build(Some(BrowserEmulation::Chrome));
        let req = FetchRequest {
            user_agent: Some("SolverUA/2.0".to_owned()),
            ..FetchRequest::new("https://example.com/x", "example")
        };
        assert_eq!(
            req.user_agent.as_deref().or(f.bot_user_agent.as_deref()),
            Some("SolverUA/2.0")
        );
    }

    #[test]
    fn every_family_resolves_to_a_profile() {
        for browser in [
            BrowserEmulation::Chrome,
            BrowserEmulation::Firefox,
            BrowserEmulation::Safari,
            BrowserEmulation::Edge,
            BrowserEmulation::OkHttp,
        ] {
            let _ = profile_for(browser);
            assert!(build(Some(browser)).bot_user_agent.is_none());
        }
    }
}
