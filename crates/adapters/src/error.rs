//! Adapter error type.

use crate::diagnostics::{describe, envelope, http_reason};
use tankovault_domain::ResolveError;
use tankovault_fetch::{FetchError, FetchResponse};
use tankovault_solver::{
    ChallengeKind, default_error_page_server, detect_challenge_body, is_rate_limit_page,
};

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
    /// The provider's own web server answered from its built-in error document, so the request
    /// never reached the site's application.
    ///
    /// The distinction this draws is the whole point. Reported as the [`Self::Http`] status it
    /// wears, a dead origin is a `404` — which reads as "this page moved", sends the next reader
    /// looking for a renamed route, and is the one verdict that is certainly wrong: the site is
    /// not being served to us at all, so no path on it would have worked either.
    #[error(
        "provider is not serving this route: its {server} answered with a built-in HTTP {status} \
         error page rather than the site ({url})"
    )]
    Unserved {
        url: String,
        status: u16,
        server: &'static str,
    },
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
        if let Some(server) = default_error_page_server(&resp.body) {
            return Self::Unserved {
                url: resp.url.clone(),
                status: resp.status,
                server,
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
    ///
    /// [`Self::Unserved`] is excluded on purpose, and it is the one exclusion that is a *policy*
    /// rather than a fact. A dead origin does recover — but not within a run, and not on any
    /// timescale the delivery ladder spans, so spending three deliveries on it only triples the
    /// requests aimed at a site that is already answering none of them. Coming back later is the
    /// scheduler's decision, taken per provider with a growing cooldown, not the task's.
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
                    || matches!(
                        source,
                        FetchError::Challenge(_)
                            | FetchError::Solver(_)
                            | FetchError::SolverUnavailable(_)
                    )
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

    /// The failure this classification exists for, as nine providers served it for a day: the
    /// origin's nginx answering `/` — a route a live site cannot 404 — from its own error page.
    /// Read as a plain `404` it says "the feed moved", which is why the first investigation went
    /// looking for a renamed route; what it actually says is that nothing on the site is being
    /// served, and the message has to say so.
    #[test]
    fn a_bare_server_error_page_is_reported_as_the_site_not_being_served() {
        let err = AdapterError::from_response(&resp(
            404,
            "<html>\n<head><title>404 Not Found</title></head>\n<body>\n\
             <center><h1>404 Not Found</h1></center>\n<hr><center>nginx</center>\n</body>\n</html>",
        ));
        assert!(
            matches!(
                err,
                AdapterError::Unserved {
                    server: "nginx",
                    status: 404,
                    ..
                }
            ),
            "expected Unserved, got {err}"
        );
        let rendered = err.to_string();
        assert!(rendered.contains("not serving this route"), "{rendered}");
        assert!(
            rendered.contains("https://example.test/manga/x/"),
            "{rendered}"
        );
        assert!(
            !err.is_transient(),
            "a dead origin does not recover inside a run; coming back later is the scheduler's \
             decision, and three deliveries only triple the requests it ignores"
        );
    }

    /// The discriminating case, and the one that must keep working: a series the provider really
    /// did remove. Its 404 comes from the application, wearing the site's own theme, and stays a
    /// plain `404` — the path is genuinely gone and the message should say that.
    #[test]
    fn a_sites_own_404_is_still_a_plain_404() {
        let themed = format!(
            "<!DOCTYPE html><html lang=\"en\"><head><title>Not found</title>\
             <link rel=\"stylesheet\" href=\"/s.css\"></head><body><nav>{}</nav>\
             <h1>We couldn't find that series</h1></body></html>",
            "<a href=\"/manga/a\">A</a>".repeat(400),
        );
        let err = AdapterError::from_response(&resp(404, &themed));
        assert!(
            matches!(err, AdapterError::Http { status: 404, .. }),
            "expected Http, got {err}"
        );
        assert!(!err.is_transient());
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
