//! Owns the fast, in-band challenge classifier (`docs/ARCHITECTURE.md` §9).
//!
//! It runs on **every** response. On a normal page the checks stop after a couple of cheap
//! comparisons (a status outside the challenge set, no Cloudflare markers), so the happy path
//! pays almost nothing. Only a positive hit triggers an expensive solve.

use crate::types::ChallengeKind;

/// Exposes the parts of a response the classifier reads.
///
/// The `fetch` layer implements it for its `FetchResponse`, so `solver` needs no dependency on
/// `fetch`.
pub trait ResponseView {
    /// Returns the HTTP status code.
    fn status(&self) -> u16;
    /// Returns a response header value by case-insensitive name, if present.
    fn header(&self, name: &str) -> Option<&str>;
    /// Returns a bounded prefix of the body as UTF-8 text, for marker scanning.
    ///
    /// Implementations should cap it, such as at the first 64 KiB, to keep the scan cheap.
    fn body_snippet(&self) -> &str;
}

/// The statuses commonly used to serve an interstitial.
const CHALLENGE_STATUSES: [u16; 3] = [403, 429, 503];

/// How far into a document the `<title>` is looked for: well past any real `<head>`, and short
/// enough to stay trivial on a multi-megabyte body.
const TITLE_SCAN_BYTES: usize = 4096;

/// Classifies a response as a bot-management interstitial from its status, headers and body
/// together.
///
/// An ordinary page, or one this cannot tell from one, is `None`. A false positive spends a
/// solve on nothing, so the markers are ones no ordinary page emits. A body with no envelope to
/// corroborate it is [`detect_challenge_body`]'s question.
pub fn detect_challenge<R: ResponseView>(resp: &R) -> Option<ChallengeKind> {
    let status = resp.status();

    // The header is the strongest and cheapest signal: a managed challenge announces itself.
    if let Some(mitigated) = resp.header("cf-mitigated")
        && mitigated.eq_ignore_ascii_case("challenge")
    {
        return Some(ChallengeKind::CloudflareManaged);
    }

    let server_is_cf = resp
        .header("server")
        .is_some_and(|v| v.eq_ignore_ascii_case("cloudflare"));

    // The common case: a status outside the challenge set with no Cloudflare server header.
    if !CHALLENGE_STATUSES.contains(&status) && !server_is_cf {
        return None;
    }

    let body = resp.body_snippet();

    // Unambiguous markup: the Turnstile widget, the JS challenge.
    if let Some(kind) = detect_challenge_body(body) {
        return Some(kind);
    }

    // A rendered rate-limit notice is the origin answering, not an interstitial. Without this
    // check, every 429 from a Cloudflare-fronted origin would reach the managed-challenge
    // fallback below and buy a pointless solve.
    if is_rate_limit_page(body) {
        return None;
    }

    // Looser body markers, admissible because the status and headers corroborate them.
    if body.contains("Just a moment") {
        return Some(ChallengeKind::CloudflareJs);
    }

    // The managed-challenge fallback for a blocked Cloudflare-fronted response.
    if server_is_cf && CHALLENGE_STATUSES.contains(&status) {
        return Some(ChallengeKind::CloudflareManaged);
    }

    // A non-Cloudflare JS interstitial: a small body that demands JavaScript.
    if CHALLENGE_STATUSES.contains(&status)
        && body.len() < 8192
        && (body.contains("enable JavaScript") || body.contains("Please turn JavaScript on"))
    {
        return Some(ChallengeKind::GenericJsInterstitial);
    }

    None
}

/// Returns whether a body is an infrastructure "too many requests" page.
///
/// A fallback for when the real status is unavailable: a solver back-end that reports no status
/// hands back a throttle notice that otherwise looks like content. Callers rebuild the `429`
/// from it, so backoff and rate limiting see the signal.
///
/// Only the title counts: "too many requests" in a body is a phrase, in a `<title>` a verdict.
#[must_use]
pub fn is_rate_limit_page(body: &str) -> bool {
    let mut cut = body.len().min(TITLE_SCAN_BYTES);
    while cut > 0 && !body.is_char_boundary(cut) {
        cut -= 1;
    }
    let head = &body[..cut];
    head.contains("<title>Too Many Requests")
        || head.contains("<title>429 Too Many Requests")
        || head.contains("<title>Rate limit exceeded")
}

/// The web servers whose built-in error document is recognised, with the marker for each.
///
/// Every marker appears in the server's compiled-in error page and in no site-authored markup:
/// nginx and its forks end theirs with `<hr><center>NAME</center>`, with an optional version,
/// and Apache and lighttpd with a signature `<address>` block.
const DEFAULT_ERROR_PAGE_MARKERS: [(&str, &str); 4] = [
    ("nginx", "<center>nginx"),
    ("openresty", "<center>openresty"),
    ("apache", "<address>Apache"),
    ("lighttpd", "<address>lighttpd"),
];

/// The largest body that can still be a server's own error document.
///
/// Real ones are hundreds of bytes, under a kilobyte even with Cloudflare's injected beacon. A
/// site's own 404 renders the site (navigation, stylesheets, a footer) and runs to tens of
/// kilobytes. The cap keeps a themed page that contains a marker from reading as an
/// infrastructure failure.
const DEFAULT_ERROR_PAGE_MAX_BYTES: usize = 4096;

/// Returns which web server answered with its built-in error document, if any.
///
/// A positive means **the request never reached the site's application**: the origin's web
/// server, or a proxy in front of it, answered from its compiled-in error page, so the status
/// describes the infrastructure, not the resource. A `404` rendered by the site says a page
/// moved; a `404` from bare nginx says the site is not being served at all, and re-requesting
/// the path cannot change it.
///
/// The examples pin that distinction:
///
/// ```
/// use tankovault_solver::default_error_page_server;
///
/// // What a dead origin behind Cloudflare returns.
/// let bare = "<html>\n<head><title>404 Not Found</title></head>\n<body>\n\
///             <center><h1>404 Not Found</h1></center>\n<hr><center>nginx</center>\n\
///             </body>\n</html>";
/// assert_eq!(default_error_page_server(bare), Some("nginx"));
///
/// // A version and platform in the signature is the same page.
/// assert_eq!(
///     default_error_page_server(
///         "<html><head><title>400 Request Header Or Cookie Too Large</title></head><body>\
///          <center><h1>400 Bad Request</h1></center><hr><center>nginx/1.18.0 (Ubuntu)</center>\
///          </body></html>"
///     ),
///     Some("nginx"),
/// );
///
/// // A site's own "this series is gone" page came from the application, however it is
/// // styled.
/// let themed = format!(
///     "<!DOCTYPE html><html><head><title>Not found</title><link rel=stylesheet href=/a.css>\
///      </head><body><nav>{}</nav><h1>Nothing here</h1></body></html>",
///     "<a href=/x>link</a>".repeat(300),
/// );
/// assert_eq!(default_error_page_server(&themed), None);
/// ```
#[must_use]
pub fn default_error_page_server(body: &str) -> Option<&'static str> {
    if body.len() > DEFAULT_ERROR_PAGE_MAX_BYTES {
        return None;
    }
    DEFAULT_ERROR_PAGE_MARKERS
        .iter()
        .find(|(_, marker)| body.contains(marker))
        .map(|(name, _)| *name)
}

/// Classifies a **body alone** as a bot-management interstitial.
///
/// Used where no status or headers corroborate the markers, chiefly the HTML a solver hands
/// back. The fetch stack would otherwise pass it to an adapter as a solved 200, and "still
/// challenged" would surface as an inscrutable parse failure.
///
/// Narrower than [`detect_challenge`] on purpose: only markup no ordinary page emits counts, so
/// a page that mentions Cloudflare (a site's FAQ, a chapter titled "Just a moment") is not a
/// challenge. The examples pin that narrowness:
///
/// ```
/// use tankovault_solver::{ChallengeKind, detect_challenge_body, is_rate_limit_page};
///
/// // Markup no content page emits. Turnstile is checked before the JS interstitial because a
/// // managed challenge serves both, and the widget is the more specific fact.
/// assert_eq!(
///     detect_challenge_body(r#"<div class="cf-turnstile" data-sitekey="x"></div>"#),
///     Some(ChallengeKind::Turnstile),
/// );
/// assert_eq!(
///     detect_challenge_body(r#"<script src="/cdn-cgi/challenge-platform/h/b/orchestrate/"></script>"#),
///     Some(ChallengeKind::CloudflareJs),
/// );
///
/// // `/cdn-cgi/challenge-platform` alone is not a challenge: the JS Detections beacon rides on
/// // ordinary content pages, and reading it as one takes a whole provider dark.
/// assert_eq!(
///     detect_challenge_body(r#"<script src="/cdn-cgi/challenge-platform/scripts/jsd/main.js">"#),
///     None,
/// );
///
/// // `"Just a moment"` counts only as a <title>; as body text it is a chapter name. The broad
/// // classifier accepts the bare phrase only because a 403/503 or a `server: cloudflare`
/// // header corroborates it, and here there is nothing to corroborate it.
/// assert_eq!(
///     detect_challenge_body("<title>Just a moment...</title>"),
///     Some(ChallengeKind::CloudflareJs),
/// );
/// assert_eq!(detect_challenge_body("<h1>Chapter 12: Just a moment</h1>"), None);
///
/// // A site's own FAQ explaining that it sits behind Cloudflare is a page, not a challenge.
/// assert_eq!(detect_challenge_body("<p>We use Cloudflare to stay online.</p>"), None);
///
/// // A throttle notice is the origin answering, not an interstitial. Two of the three
/// // challenge statuses are also rate-limit statuses, and a solve here would only fetch the
/// // same notice again.
/// let throttled = "<html><head><title>429 Too Many Requests</title></head></html>";
/// assert_eq!(detect_challenge_body(throttled), None);
/// assert!(is_rate_limit_page(throttled));
/// ```
#[must_use]
pub fn detect_challenge_body(body: &str) -> Option<ChallengeKind> {
    if body.contains("challenges.cloudflare.com/turnstile") || body.contains("cf-turnstile") {
        return Some(ChallengeKind::Turnstile);
    }
    if loads_challenge_orchestration(body)
        || body.contains("cf_chl_opt")
        || body.contains("<title>Just a moment")
    {
        return Some(ChallengeKind::CloudflareJs);
    }
    None
}

/// Returns whether the body loads the interstitial's **orchestration** script.
///
/// `/cdn-cgi/challenge-platform` alone is never a challenge marker: Cloudflare injects its JS
/// Detections beacon (`/cdn-cgi/challenge-platform/scripts/jsd/main.js`) into ordinary pages on
/// every zone with the feature on. Only the orchestration entry point, `…/orchestrate/` or its
/// `chl_page` variant, belongs to the interstitial.
fn loads_challenge_orchestration(body: &str) -> bool {
    body.contains("/cdn-cgi/challenge-platform")
        && (body.contains("/orchestrate/") || body.contains("chl_page"))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Resp {
        status: u16,
        headers: Vec<(String, String)>,
        body: String,
    }
    impl ResponseView for Resp {
        fn status(&self) -> u16 {
            self.status
        }
        fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.as_str())
        }
        fn body_snippet(&self) -> &str {
            &self.body
        }
    }

    fn resp(status: u16, headers: &[(&str, &str)], body: &str) -> Resp {
        Resp {
            status,
            headers: headers
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
            body: body.to_owned(),
        }
    }

    #[test]
    fn normal_page_is_not_a_challenge() {
        assert_eq!(detect_challenge(&resp(200, &[], "<html>ok</html>")), None);
    }

    #[test]
    fn managed_challenge_via_header() {
        let r = resp(
            403,
            &[("cf-mitigated", "challenge"), ("server", "cloudflare")],
            "",
        );
        assert_eq!(detect_challenge(&r), Some(ChallengeKind::CloudflareManaged));
    }

    #[test]
    fn js_challenge_via_body() {
        let r = resp(
            503,
            &[("server", "cloudflare")],
            "<title>Just a moment...</title><script src=/cdn-cgi/challenge-platform/x></script>",
        );
        assert_eq!(detect_challenge(&r), Some(ChallengeKind::CloudflareJs));
    }

    #[test]
    fn turnstile_detected() {
        let r = resp(
            403,
            &[("server", "cloudflare")],
            "<div class=cf-turnstile></div>",
        );
        assert_eq!(detect_challenge(&r), Some(ChallengeKind::Turnstile));
    }

    #[test]
    fn generic_js_interstitial() {
        let r = resp(403, &[], "Please enable JavaScript to continue");
        assert_eq!(
            detect_challenge(&r),
            Some(ChallengeKind::GenericJsInterstitial)
        );
    }

    #[test]
    fn body_only_detection_finds_the_unambiguous_markers() {
        assert_eq!(
            detect_challenge_body("<div class=\"cf-turnstile\"></div>"),
            Some(ChallengeKind::Turnstile)
        );
        assert_eq!(
            detect_challenge_body(
                "<script src=\"/cdn-cgi/challenge-platform/h/b/orchestrate/chl_page/v1?ray=x\">\
                 </script>"
            ),
            Some(ChallengeKind::CloudflareJs)
        );
        assert_eq!(
            detect_challenge_body("<html><head><title>Just a moment...</title></head></html>"),
            Some(ChallengeKind::CloudflareJs)
        );
    }

    #[test]
    fn a_rate_limited_origin_is_not_mistaken_for_a_challenge() {
        // 429 is both a rate-limit and a challenge status, and this origin is behind
        // Cloudflare: without the body check it would reach the managed-challenge fallback.
        let r = resp(
            429,
            &[("server", "cloudflare"), ("retry-after", "60")],
            "<html lang=\"en\"><head><meta charset=\"utf-8\">\
             <title>Too Many Requests</title></head><body></body></html>",
        );
        assert_eq!(detect_challenge(&r), None);
    }

    #[test]
    fn a_rendered_throttle_notice_is_recognised() {
        let page = "<html lang=\"en\"><head> <meta charset=\"utf-8\"> \
                    <title>Too Many Requests</title> </head><body></body></html>";
        assert!(is_rate_limit_page(page));
        // The phrase in page content is not a verdict; only the title is.
        assert!(!is_rate_limit_page(
            "<html><head><title>Chapter 4</title></head><body>Too Many Requests, he thought.\
             </body></html>"
        ));
        assert!(!is_rate_limit_page("{\"data\":{\"chapters\":[]}}"));
    }

    #[test]
    fn body_only_detection_does_not_fire_on_ordinary_content() {
        // With nothing to corroborate it, a page that says the words is not an interstitial;
        // callers turn a positive into a failed fetch.
        assert_eq!(
            detect_challenge_body("<h1>Chapter 12: Just a moment longer</h1>"),
            None
        );
        assert_eq!(detect_challenge_body("{\"data\":{\"chapters\":[]}}"), None);
    }

    /// The JS Detections beacon Cloudflare injects into **content** pages is not an interstitial.
    ///
    /// The bug: matching `/cdn-cgi/challenge-platform` anywhere classifies every page of a zone
    /// with the feature on as a challenge. The direct `200` buys a pointless solve, and the
    /// solver's page is then rejected as "still challenged".
    #[test]
    fn the_js_detections_beacon_is_not_a_challenge() {
        // Verbatim from a `WeebCentral` series page, served as a 200 with the full document.
        let page = "<html><head><script>window.__CF$cv$params={r:'a2a0914e',t:'MTc4Ng=='};\
                    var a=document.createElement('script');\
                    a.src='/cdn-cgi/challenge-platform/scripts/jsd/main.js';</script></head>\
                    <body><h1>Atsu Atsu Trattoria</h1></body></html>";
        assert_eq!(detect_challenge_body(page), None);

        // In band too, where the `server: cloudflare` header keeps the body scan running, as on
        // a real fetch of that page.
        let r = resp(200, &[("server", "cloudflare")], page);
        assert_eq!(detect_challenge(&r), None);
    }

    /// Bare server error documents are recognised as infrastructure.
    ///
    /// The bodies are byte-for-byte what live origins serve: Keyoapp installs answering every
    /// route, `/` included, with bare nginx, and an origin refusing an oversized header the
    /// same way. Read as ordinary statuses, all of them say "the page is gone".
    #[test]
    fn a_bare_server_error_document_is_recognised_as_infrastructure() {
        assert_eq!(
            default_error_page_server(
                "<html>\n<head><title>404 Not Found</title></head>\n<body>\n\
                 <center><h1>404 Not Found</h1></center>\n<hr><center>nginx</center>\n\
                 </body>\n</html>\n\
                 <!-- a padding to disable MSIE and Chrome friendly error page -->"
            ),
            Some("nginx")
        );
        assert_eq!(
            default_error_page_server(
                "<html>\n<head><title>404 Not Found</title></head>\n<body>\n\
                 <center><h1>404 Not Found</h1></center>\n<hr><center>nginx</center>\n\
                 <script type=\"module\" src=\"https://static.cloudflareinsights.com/beacon.min.js\
                 \" data-cf-beacon='{\"version\":\"2024.11.0\"}'></script>\n</body>\n</html>"
            ),
            Some("nginx"),
            "a Cloudflare beacon injected into the page does not make it the site's own"
        );
        assert_eq!(
            default_error_page_server(
                "<html> <head><title>400 Request Header Or Cookie Too Large</title></head> \
                 <body> <center><h1>400 Bad Request</h1></center> \
                 <center>Request Header Or Cookie Too Large</center> \
                 <hr><center>nginx/1.18.0 (Ubuntu)</center> </body> </html>"
            ),
            Some("nginx")
        );
    }

    /// A site's own 404 for a removed series stays a permanent `404`.
    ///
    /// The site renders it through its theme, which is large and carries no server signature,
    /// and the path really is gone.
    #[test]
    fn a_sites_own_error_page_is_not_an_infrastructure_failure() {
        let themed = format!(
            "<!DOCTYPE html> <html lang=\"en\"><head><title>Page not found</title>\
             <link rel=\"stylesheet\" href=\"/style.css\"></head><body><header>{}</header>\
             <main><h1>We couldn't find that series</h1></main></body></html>",
            "<a href=\"/manga/x\">Some Series</a>".repeat(400),
        );
        assert!(themed.len() > DEFAULT_ERROR_PAGE_MAX_BYTES);
        assert_eq!(default_error_page_server(&themed), None);

        // The same holds for a short page with no server signature.
        assert_eq!(
            default_error_page_server("<html><body><h1>404</h1><p>Gone.</p></body></html>"),
            None
        );
    }

    /// A page under the cap that mentions a server name is not its error document: the markers
    /// are the compiled-in markup, not the word.
    #[test]
    fn naming_a_server_is_not_the_same_as_being_its_error_page() {
        assert_eq!(
            default_error_page_server("<p>We run nginx and Apache. Just a moment.</p>"),
            None
        );
        assert_eq!(default_error_page_server("{\"error\":\"nginx\"}"), None);
    }

    #[test]
    fn forbidden_without_cf_markers_is_not_a_false_positive() {
        // A plain 403 from a normal server with an ordinary body is not a challenge.
        let r = resp(403, &[("server", "nginx")], "<h1>403 Forbidden</h1>");
        assert_eq!(detect_challenge(&r), None);
    }
}
