//! Defines the request and response types that cross the fetch stack.

use tankovault_solver::ResponseView;

/// The most body bytes the classifier scans for challenge markers.
const SNIPPET_CAP: usize = 64 * 1024;

/// A GET request through the fetch stack.
///
/// Only GET exists: this system never posts to providers, and there is no image or binary fetch
/// path.
#[derive(Debug, Clone)]
pub struct FetchRequest {
    /// The absolute target URL.
    pub url: String,
    /// The provider slug, which keys the rate limiter and the solved-session cache.
    pub provider_slug: String,
    /// The extra request headers.
    pub headers: Vec<(String, String)>,
    /// An overriding user agent, such as a solved session's; the stack default otherwise.
    pub user_agent: Option<String>,
}

impl FetchRequest {
    /// Builds a bare GET for `url`, attributed to `provider_slug`.
    #[must_use]
    pub fn new(url: impl Into<String>, provider_slug: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            provider_slug: provider_slug.into(),
            headers: Vec::new(),
            user_agent: None,
        }
    }

    /// Adds a request header.
    #[must_use]
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
}

/// A response whose `body` is bounded UTF-8 text (HTML or JSON), never binary content.
#[derive(Debug, Clone)]
pub struct FetchResponse {
    /// The HTTP status code.
    pub status: u16,
    /// The final URL after any redirects.
    pub url: String,
    /// The response headers.
    pub headers: Vec<(String, String)>,
    /// The bounded response body as text.
    pub body: String,
    /// Whether the conditional body cache served this response.
    pub from_cache: bool,
}

impl FetchResponse {
    /// Returns a header value by case-insensitive name.
    ///
    /// HTTP/2 lower-cases header names and HTTP/1.1 does not, so a case-sensitive match would
    /// read `Retry-After` on one protocol and miss it on the other.
    ///
    /// ```
    /// use tankovault_fetch::FetchResponse;
    ///
    /// let resp = FetchResponse {
    ///     status: 429,
    ///     url: "https://provider.test/manga/x/".to_owned(),
    ///     headers: vec![
    ///         ("Retry-After".to_owned(), "120".to_owned()),
    ///         ("server".to_owned(), "cloudflare".to_owned()),
    ///         // Providers do repeat headers. `header` answers with the first.
    ///         ("retry-after".to_owned(), "5".to_owned()),
    ///     ],
    ///     body: String::new(),
    ///     from_cache: false,
    /// };
    ///
    /// assert_eq!(resp.header("retry-after"), Some("120"));
    /// assert_eq!(resp.header("RETRY-AFTER"), Some("120"));
    /// assert_eq!(resp.header("Server"), Some("cloudflare"));
    /// assert_eq!(resp.header("x-absent"), None);
    ///
    /// // `is_success` is strictly 2xx, so a `304` is not success. The stack turns a validated
    /// // cache hit into the cached `200` with `from_cache` set, so a `304` reaching an adapter
    /// // means the validation path is broken.
    /// assert!(resp.header("retry-after").is_some() && !resp.is_success());
    /// assert!(!FetchResponse { status: 304, ..resp.clone() }.is_success());
    /// assert!(FetchResponse { status: 204, ..resp }.is_success());
    /// ```
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// Returns whether the status is 2xx.
    #[must_use]
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

impl ResponseView for FetchResponse {
    fn status(&self) -> u16 {
        self.status
    }
    fn header(&self, name: &str) -> Option<&str> {
        FetchResponse::header(self, name)
    }
    fn body_snippet(&self) -> &str {
        let end = self.body.len().min(SNIPPET_CAP);
        // Respect UTF-8 boundaries when truncating the scan window.
        let mut cut = end;
        while cut > 0 && !self.body.is_char_boundary(cut) {
            cut -= 1;
        }
        &self.body[..cut]
    }
}
