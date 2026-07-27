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
    /// Carries the URL it was aimed at: the transport reports *what* went wrong ("request
    /// timed out"), never *where*, and one page of a scan is indistinguishable from the next
    /// without it.
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
    /// Built by [`AdapterError::from_response`], which quotes the refusal back: a `404` on a
    /// stale path, a soft `404` that is really a block page, and a `403` interstitial all read
    /// identically as a bare status.
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
    /// The provider served a bot-management interstitial in place of the document, and it
    /// reached the adapter — i.e. the fetch stack could not solve it away.
    ///
    /// Distinct from [`Self::Parse`] on purpose: the body is not malformed, it is somebody
    /// else's page. It is also retryable, whereas a genuine parse failure is not.
    #[error("provider served a {kind} challenge page instead of content: {url}")]
    Challenged { url: String, kind: ChallengeKind },
    /// The provider served a rate-limit notice in place of the document.
    ///
    /// Reached only when the notice arrived with a success status — a provider (or a solver
    /// back-end) that says `200` while rendering "Too Many Requests". The page is the
    /// evidence; a `429` that arrives labelled as one never gets this far.
    #[error("provider served a rate-limit page instead of content: {url}")]
    Throttled { url: String },
    /// No adapter is registered for a `custom` provider slug.
    #[error("no custom adapter registered for provider {0:?}")]
    UnknownCustom(String),
}

impl AdapterError {
    /// Classify a non-success response.
    ///
    /// A refusal is not always the status it arrives as. The fetch stack turns an
    /// interstitial or a throttle notice into the right status *when something upstream
    /// reports one honestly*; when nothing does, the body is the only evidence left, and a
    /// `403` challenge page recorded as a plain `403` is both unreadable and misclassified —
    /// it would be retried never, when it is exactly the failure a later attempt clears.
    ///
    /// The same backstop the parse path applies (see [`crate::json`]), on the status path.
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
    /// Inspects the body before blaming the selector: an interstitial or a throttle notice
    /// served as `200` parses perfectly and matches nothing, and calling that "required
    /// element not found" sends the reader hunting markup that never changed. What is left is
    /// a real markup mismatch, reported with the page that produced it.
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
    /// This is the scan scheduler's question, not the fetch stack's: it decides whether a
    /// failed scan task is worth redelivering minutes later, so it counts the failures that
    /// pass with time — an unsolved challenge, a solver outage, rate limiting, a provider
    /// 5xx — and excludes anything that will fail identically on replay (a selector that no
    /// longer matches, a malformed body, a bad config).
    ///
    /// Deliberately wider than [`FetchError::is_transient`], which governs *immediate*
    /// in-stack retries and therefore excludes challenge/solver failures that must not be
    /// hammered inside one request.
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
