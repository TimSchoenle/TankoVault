//! Adapter error type.

use crate::diagnostics::{describe, envelope, http_reason};
use tankovault_domain::ResolveError;
use tankovault_fetch::{FetchError, FetchResponse};
use tankovault_solver::{ChallengeKind, detect_challenge_body, is_rate_limit_page};

/// Errors raised while enumerating or parsing a provider.
#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    /// A fetch through the injected stack failed.
    ///
    /// Carries the URL: the transport reports what failed, never where.
    #[error("fetch of {url} failed: {source}")]
    Fetch {
        url: String,
        #[source]
        source: FetchError,
    },
    /// A relative path could not be resolved against the provider base URL.
    #[error(transparent)]
    Resolve(#[from] ResolveError),
    /// The provider returned a non-success status for a page.
    ///
    /// Built by [`AdapterError::from_response`], which reclassifies challenge/throttle bodies
    /// served under a success status before falling back to this.
    #[error("provider returned HTTP {status} {} for {url} ({envelope})", http_reason(*.status))]
    Http {
        status: u16,
        url: String,
        envelope: String,
    },
    /// A CSS selector in the adapter config was invalid.
    #[error("invalid selector {selector:?}: {reason}")]
    Selector { selector: String, reason: String },
    /// The adapter config JSON did not match the expected schema.
    #[error("invalid adapter config: {0}")]
    Config(String),
    /// A required element was absent from the parsed page.
    #[error("required element not found: {0}")]
    Missing(String),
    /// A structured (e.g. JSON API) response could not be parsed.
    #[error("failed to parse provider response: {0}")]
    Parse(String),
    /// A bot-management interstitial reached the adapter: the fetch stack could not solve it.
    ///
    /// Distinct from [`Self::Parse`]: the body is somebody else's page, not malformed data, so
    /// it is retryable where a genuine parse failure is not.
    #[error("provider served a {kind} challenge page instead of content: {url}")]
    Challenged { url: String, kind: ChallengeKind },
    /// A rate-limit notice reached the adapter under a success status; a `429` labelled
    /// honestly never gets this far.
    #[error("provider served a rate-limit page instead of content: {url}")]
    Throttled { url: String },
    /// No adapter is registered for a `custom` provider slug.
    #[error("no custom adapter registered for provider {0:?}")]
    UnknownCustom(String),
}

impl AdapterError {
    /// Classify a non-success response.
    ///
    /// The fetch stack relabels an interstitial/throttle body only when something upstream
    /// reports it honestly; when nothing does, a `403` challenge page recorded as a plain
    /// `403` would never be retried, so the body is checked here as the last resort.
    #[must_use]
    pub fn from_response(resp: &FetchResponse) -> Self {
        if let Some(kind) = detect_challenge_body(&resp.body) {
            return Self::Challenged {
                url: resp.url.clone(),
                kind,
            };
        }
        if is_rate_limit_page(&resp.body) {
            return Self::Throttled {
                url: resp.url.clone(),
            };
        }
        Self::Http {
            status: resp.status,
            url: resp.url.clone(),
            envelope: envelope(resp),
        }
    }

    /// A required element was absent from an otherwise successful response.
    ///
    /// Checks for a challenge/throttle body first: a `200` interstitial parses cleanly and
    /// matches no selector, and reporting that as "not found" sends the reader chasing
    /// markup that never changed.
    #[must_use]
    pub fn missing(what: &str, resp: &FetchResponse) -> Self {
        if let Some(kind) = detect_challenge_body(&resp.body) {
            return Self::Challenged {
                url: resp.url.clone(),
                kind,
            };
        }
        if is_rate_limit_page(&resp.body) {
            return Self::Throttled {
                url: resp.url.clone(),
            };
        }
        Self::Missing(format!("{what}; {}", describe(resp)))
    }

    /// Whether a **later** attempt could plausibly succeed.
    ///
    /// Wider than [`FetchError::is_transient`] (immediate in-stack retries only): this also
    /// counts an unsolved challenge, a solver outage and rate limiting as worth redelivering,
    /// and excludes anything that repeats identically on replay (stale selector, malformed
    /// body, bad config).
    #[must_use]
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Challenged { .. }
            | Self::Throttled { .. }
            | Self::Http {
                status: 429 | 500..=599,
                ..
            } => true,
            Self::Fetch { source, .. } => {
                source.is_transient()
                    || matches!(source, FetchError::Challenge(_) | FetchError::Solver(_))
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resp(status: u16, body: &str) -> FetchResponse {
        FetchResponse {
            status,
            url: "https://example.test/manga/x/".to_owned(),
            headers: vec![("content-type".to_owned(), "text/html".to_owned())],
            body: body.to_owned(),
            from_cache: false,
        }
    }

    #[test]
    fn a_refusal_names_the_page_it_refused() {
        let err = AdapterError::from_response(&resp(
            404,
            "<!DOCTYPE html><title>Page not found</title><p>The manga you requested is gone.",
        ));
        let rendered = err.to_string();
        assert!(rendered.contains("HTTP 404 Not Found"), "{rendered}");
        assert!(
            rendered.contains("https://example.test/manga/x/"),
            "the URL is the actionable part: {rendered}"
        );
        assert!(
            rendered.contains("Page not found"),
            "the refusal is quoted back: {rendered}"
        );
        assert!(!err.is_transient(), "a 404 will 404 again");
    }

    #[test]
    fn an_interstitial_is_reported_as_one_whatever_status_it_wore() {
        let err = AdapterError::from_response(&resp(
            403,
            "<html><head><title>Just a moment...</title></head>\
             <body><div class=\"cf-turnstile\"></div></body></html>",
        ));
        assert!(
            matches!(err, AdapterError::Challenged { .. }),
            "expected a challenge, got {err}"
        );
        assert!(err.is_transient(), "a challenge is worth another delivery");
    }

    #[test]
    fn a_throttle_notice_is_reported_as_throttling() {
        let err = AdapterError::from_response(&resp(
            429,
            "<html><head><title>Too Many Requests</title></head><body></body></html>",
        ));
        assert!(
            matches!(err, AdapterError::Throttled { .. }),
            "expected throttling, got {err}"
        );
    }

    #[test]
    fn a_missing_element_carries_the_page_that_lacked_it() {
        let err = AdapterError::missing(
            "series title (selector \"h1.entry-title\")",
            &resp(
                200,
                "<html><body><h1 class=\"renamed\">Berserk</h1></body></html>",
            ),
        );
        let rendered = err.to_string();
        assert!(rendered.contains("h1.entry-title"), "{rendered}");
        assert!(
            rendered.contains("url=https://example.test/manga/x/"),
            "{rendered}"
        );
        assert!(
            !err.is_transient(),
            "markup drift fails identically on replay"
        );
    }

    #[test]
    fn a_missing_element_on_an_interstitial_blames_the_interstitial() {
        let err = AdapterError::missing(
            "series title",
            &resp(
                200,
                "<html><head><title>Just a moment...</title></head>\
                 <body><div class=\"cf-turnstile\"></div></body></html>",
            ),
        );
        assert!(
            matches!(err, AdapterError::Challenged { .. }),
            "an unsolved challenge is not a selector problem: {err}"
        );
    }
}
