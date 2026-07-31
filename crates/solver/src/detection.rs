//! The fast, in-band challenge classifier (design §9).
//!
//! Runs on **every** response. On a normal page the checks short-circuit after a couple
//! of cheap comparisons (status not in the challenge set, no Cloudflare markers), so the
//! happy path pays almost nothing. Only a positive hit triggers an (expensive) solve.

use crate::types::ChallengeKind;

/// The minimal view of a response the classifier needs. The `fetch` layer implements
/// this for its `FetchResponse`, so `solver` needs no dependency on `fetch`.
pub trait ResponseView {
    /// HTTP status code.
    fn status(&self) -> u16;
    /// A response header value by (case-insensitive) name, if present.
    fn header(&self, name: &str) -> Option<&str>;
    /// A bounded prefix of the response body as UTF-8 text, for marker scanning.
    /// Implementations should cap this (e.g. first 64 KiB) to keep the scan cheap.
    fn body_snippet(&self) -> &str;
}

/// Statuses commonly used to serve an interstitial/challenge.
const CHALLENGE_STATUSES: [u16; 3] = [403, 429, 503];

/// How far into a document the `<title>` is looked for. Well past any real `<head>`, short
/// enough that the scan stays trivial on a multi-megabyte body.
const TITLE_SCAN_BYTES: usize = 4096;

pub fn detect_challenge<R: ResponseView>(resp: &R) -> Option<ChallengeKind> {
    let status = resp.status();

    // Header signal is the strongest and cheapest: a managed challenge announces itself.
    if let Some(mitigated) = resp.header("cf-mitigated") {
        if mitigated.eq_ignore_ascii_case("challenge") {
            return Some(ChallengeKind::CloudflareManaged);
        }
    }

    let server_is_cf = resp
        .header("server")
        .is_some_and(|v| v.eq_ignore_ascii_case("cloudflare"));

    // On a normal 2xx with no Cloudflare server header, stop here — the common case.
    if !CHALLENGE_STATUSES.contains(&status) && !server_is_cf {
        return None;
    }

    let body = resp.body_snippet();

    // Unambiguous markup markers (Turnstile widget, classic JS challenge).
    if let Some(kind) = detect_challenge_body(body) {
        return Some(kind);
    }

    // A rendered rate-limit notice is the origin answering, not an interstitial in front of
    // it — and two of the three challenge statuses are also the rate-limit statuses. Without
    // this, every `429` from a Cloudflare-fronted origin fell through to the managed-challenge
    // fallback below and bought an expensive solve, whose only possible result was fetching
    // the same notice again through a browser: the provider's `Retry-After` was replaced by a
    // solver-invented `200`, and the layers that exist to slow the crawl down never fired.
    if is_rate_limit_page(body) {
        return None;
    }

    // Looser body markers, admissible here because the status/header envelope already
    // corroborates them.
    if body.contains("Just a moment") {
        return Some(ChallengeKind::CloudflareJs);
    }

    // Managed challenge fallback on a Cloudflare-fronted forbidden/blocked response.
    if server_is_cf && CHALLENGE_STATUSES.contains(&status) {
        return Some(ChallengeKind::CloudflareManaged);
    }

    // Generic (non-Cloudflare) JS interstitial: a small body that demands JavaScript.
    if CHALLENGE_STATUSES.contains(&status)
        && body.len() < 8192
        && (body.contains("enable JavaScript") || body.contains("Please turn JavaScript on"))
    {
        return Some(ChallengeKind::GenericJsInterstitial);
    }

    None
}

/// Whether a body is an infrastructure "you are sending too many requests" page.
///
/// A fallback for the case where the real status is unavailable — a solver back-end that
/// reports no status hands back a rendered throttle notice that is otherwise indistinguishable
/// from content. Callers use it to reconstruct the `429` the provider actually sent, so the
/// backoff and rate-limiting layers see the signal they exist for.
///
/// Matches on the document title only, because "too many requests" is a phrase, while a
/// `<title>` saying it is a verdict.
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

/// Classify a **body alone** as a bot-management interstitial.
///
/// Used where there is no status/header envelope to corroborate the markers — chiefly the
/// HTML a challenge solver hands back, which the fetch stack would otherwise accept as a
/// solved 200 and pass to an adapter, turning "still challenged" into an inscrutable parse
/// failure downstream.
///
/// Deliberately narrower than [`detect_challenge`]: only markup that no ordinary page emits
/// counts here, so a page that merely *mentions* Cloudflare (a scanlation site's FAQ, a
/// chapter titled "Just a moment") is not misread as a challenge.
///
/// The narrowness is the contract, so it is what the examples pin:
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
/// // `"Just a moment"` counts only as a <title>. As body text it is a chapter name, and this
/// // is the case that looks like a missed detection: the broader classifier accepts the bare
/// // phrase, but only because a 403/503 or a `server: cloudflare` header has already
/// // corroborated it. Here there is no envelope to corroborate anything.
/// assert_eq!(
///     detect_challenge_body("<title>Just a moment...</title>"),
///     Some(ChallengeKind::CloudflareJs),
/// );
/// assert_eq!(detect_challenge_body("<h1>Chapter 12: Just a moment</h1>"), None);
///
/// // A site's own FAQ explaining that it sits behind Cloudflare is a page, not a challenge.
/// assert_eq!(detect_challenge_body("<p>We use Cloudflare to stay online.</p>"), None);
///
/// // And a throttle notice is the origin answering, not an interstitial in front of it. Two of
/// // the three challenge statuses are also the rate-limit statuses, so conflating these buys
/// // an expensive solve whose only possible result is fetching the same notice again.
/// let throttled = "<html><head><title>429 Too Many Requests</title></head></html>";
/// assert_eq!(detect_challenge_body(throttled), None);
/// assert!(is_rate_limit_page(throttled));
/// ```
#[must_use]
pub fn detect_challenge_body(body: &str) -> Option<ChallengeKind> {
    if body.contains("challenges.cloudflare.com/turnstile") || body.contains("cf-turnstile") {
        return Some(ChallengeKind::Turnstile);
    }
    if body.contains("/cdn-cgi/challenge-platform")
        || body.contains("cf_chl_opt")
        || body.contains("<title>Just a moment")
    {
        return Some(ChallengeKind::CloudflareJs);
    }
    None
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
            detect_challenge_body("<script src=\"/cdn-cgi/challenge-platform/h/b/x\"></script>"),
            Some(ChallengeKind::CloudflareJs)
        );
        assert_eq!(
            detect_challenge_body("<html><head><title>Just a moment...</title></head></html>"),
            Some(ChallengeKind::CloudflareJs)
        );
    }

    #[test]
    fn a_rate_limited_origin_is_not_mistaken_for_a_challenge() {
        // 429 is both a rate-limit status and a challenge status, and this origin sits behind
        // Cloudflare — so without the body check this fell through to the managed-challenge
        // fallback and spent a solve on a page that only says "slow down".
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
        // The phrase in page content is not a verdict — only the title is.
        assert!(!is_rate_limit_page(
            "<html><head><title>Chapter 4</title></head><body>Too Many Requests, he thought.\
             </body></html>"
        ));
        assert!(!is_rate_limit_page("{\"data\":{\"chapters\":[]}}"));
    }

    #[test]
    fn body_only_detection_does_not_fire_on_ordinary_content() {
        // Without a status or header to corroborate it, a page that merely says the words
        // must not be mistaken for an interstitial — this classifier's callers turn a
        // positive into a failed fetch.
        assert_eq!(
            detect_challenge_body("<h1>Chapter 12: Just a moment longer</h1>"),
            None
        );
        assert_eq!(detect_challenge_body("{\"data\":{\"chapters\":[]}}"), None);
    }

    #[test]
    fn forbidden_without_cf_markers_is_not_a_false_positive() {
        // A plain 403 from a normal server with an ordinary body is not a challenge.
        let r = resp(403, &[("server", "nginx")], "<h1>403 Forbidden</h1>");
        assert_eq!(detect_challenge(&r), None);
    }
}
