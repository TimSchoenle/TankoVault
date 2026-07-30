//! Request/response value types crossing the fetch stack.

use tankovault_solver::ResponseView;

/// Max bytes of a response body the classifier scans for challenge markers.
const SNIPPET_CAP: usize = 64 * 1024;

/// A GET request through the fetch stack. (Only GET is exposed — this system never POSTs
/// to providers, and there is no image/binary fetch path.)
#[derive(Debug, Clone)]
pub struct FetchRequest {
    /// Absolute target URL.
    pub url: String,
    /// Provider slug — keys the rate limiter and solved-session cache.
    pub provider_slug: String,
    /// Extra request headers.
    pub headers: Vec<(String, String)>,
    /// Overriding user-agent (e.g. a solved-session UA); falls back to the stack default.
    pub user_agent: Option<String>,
}

impl FetchRequest {
    /// A bare GET for `url` attributed to `provider_slug`.
    #[must_use]
    pub fn new(url: impl Into<String>, provider_slug: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            provider_slug: provider_slug.into(),
            headers: Vec::new(),
            user_agent: None,
        }
    }

    /// Add a request header.
    #[must_use]
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
}

/// A response. `body` is bounded UTF-8 text (HTML/JSON) — never binary/image content.
#[derive(Debug, Clone)]
pub struct FetchResponse {
    /// HTTP status code.
    pub status: u16,
    /// Final URL after any redirects.
    pub url: String,
    /// Response headers.
    pub headers: Vec<(String, String)>,
    /// Bounded response body as text.
    pub body: String,
    /// Whether this response was served from the conditional/body cache.
    pub from_cache: bool,
}

impl FetchResponse {
    /// A header value by case-insensitive name.
    ///
    /// Case-insensitive because the name is compared against whatever casing the *provider*
    /// sent, and HTTP/2 lowercases while HTTP/1.1 does not — so a stack that matched
    /// case-sensitively would read `Retry-After` from one protocol version and miss it on the
    /// other, silently disabling the backoff layer for half the fleet.
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
    /// // `is_success` is strictly 2xx, so a redirect is *not* success even though it carries no
    /// // error. That looks harsh for a `304`, and is the point: `from_cache` is how a
    /// // cache-validated response is recognised, and the stack normalises the status to the
    /// // cached `200` before an adapter ever sees it. A `304` reaching an adapter means the
    /// // validation path is broken, so it must not read as success.
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

    /// Whether the status is 2xx.
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
