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

/// Classify a response as a bot-management challenge, or `None` for a normal page.
///
/// Ordered cheapest-first: status, then headers, then a bounded body scan.
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

    // Turnstile widget.
    if body.contains("challenges.cloudflare.com/turnstile") || body.contains("cf-turnstile") {
        return Some(ChallengeKind::Turnstile);
    }

    // Classic JS challenge.
    if body.contains("/cdn-cgi/challenge-platform")
        || body.contains("cf_chl_opt")
        || body.contains("Just a moment")
    {
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
    fn forbidden_without_cf_markers_is_not_a_false_positive() {
        // A plain 403 from a normal server with an ordinary body is not a challenge.
        let r = resp(403, &[("server", "nginx")], "<h1>403 Forbidden</h1>");
        assert_eq!(detect_challenge(&r), None);
    }
}
