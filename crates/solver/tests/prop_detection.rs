//! Properties of the in-band challenge classifier.
//!
//! The classifier runs on every response, so its input is provider- or solver-controlled and
//! includes bodies that are not markup at all. Two properties matter that no example test
//! checks:
//!
//! 1. It cannot panic. `is_rate_limit_page` cuts a prefix by byte count and walks back with
//!    `is_char_boundary`; a property over arbitrary Unicode keeps that correct.
//! 2. The narrow body-only classifier never accepts what the envelope-aware one rejects.
//!    Otherwise the fetch stack would report "still challenged" for a page the same code
//!    considers ordinary.

use proptest::prelude::*;
use tankovault_solver::{
    ResponseView, detect_challenge, detect_challenge_body, is_rate_limit_page,
};

/// A minimal response the test controls.
struct Resp {
    status: u16,
    server: Option<String>,
    body: String,
}

impl ResponseView for Resp {
    fn status(&self) -> u16 {
        self.status
    }
    fn header(&self, name: &str) -> Option<&str> {
        if name.eq_ignore_ascii_case("server") {
            self.server.as_deref()
        } else {
            None
        }
    }
    fn body_snippet(&self) -> &str {
        &self.body
    }
}

/// Generates bodies from the markers the classifier looks for, mixed with arbitrary text.
///
/// Uniform random strings almost never contain `cf-turnstile` or `<title>Too Many Requests`, so
/// a plain `".*"` strategy would only exercise the negative path.
fn marker_body() -> impl Strategy<Value = String> {
    let fragment = prop::sample::select(vec![
        "cf-turnstile",
        "challenges.cloudflare.com/turnstile",
        "/cdn-cgi/challenge-platform/h/b/orchestrate/chl_page/v1?ray=x",
        // The JS Detections beacon rides on ordinary pages: it shares the interstitial's path
        // prefix and must not decide the outcome on its own.
        "/cdn-cgi/challenge-platform/scripts/jsd/main.js",
        "cf_chl_opt",
        "<title>Just a moment",
        "Just a moment",
        "<title>Too Many Requests",
        "<title>429 Too Many Requests",
        "<title>Rate limit exceeded",
        "enable JavaScript",
        "Please turn JavaScript on",
        "<html><body>an ordinary chapter page</body></html>",
        "İstanbul ünicode ẞ \u{0301}combining",
        "",
    ]);
    prop::collection::vec(fragment, 0..6).prop_map(|parts| parts.concat())
}

proptest! {
    /// Totality over arbitrary Unicode, including the multi-byte sequences that straddle the
    /// 4096-byte title-scan cut.
    #[test]
    fn the_body_only_classifiers_are_total(body in ".*") {
        let _ = detect_challenge_body(&body);
        let _ = is_rate_limit_page(&body);
    }

    /// The title-scan cut never splits a character.
    ///
    /// A run of three-byte characters long enough to be truncated cannot be cut at 4096 without
    /// the backwards walk.
    #[test]
    fn the_title_scan_cut_never_splits_a_character(repeats in 1350usize..1400usize) {
        let body = "€".repeat(repeats);
        let _ = is_rate_limit_page(&body);
    }

    /// Totality of the envelope-aware classifier over any status and any body.
    #[test]
    fn the_envelope_classifier_is_total(
        status in any::<u16>(),
        server in prop::option::of("[a-zA-Z-]{0,20}"),
        body in ".*",
    ) {
        let _ = detect_challenge(&Resp { status, server, body });
    }

    /// Whatever the narrow classifier accepts, the broad one accepts too, given an envelope that
    /// lets it read the body.
    ///
    /// If this fails, the fetch stack and the adapter layer disagree about the same bytes, and
    /// the symptom is an inscrutable parse failure instead of "still challenged".
    #[test]
    fn whatever_the_body_classifier_accepts_the_envelope_classifier_also_accepts(
        body in marker_body(),
    ) {
        prop_assume!(detect_challenge_body(&body).is_some());
        let resp = Resp {
            status: 403,
            server: Some("cloudflare".to_owned()),
            body,
        };
        prop_assert!(
            detect_challenge(&resp).is_some(),
            "the narrow classifier accepted a body the broad one calls clean"
        );
    }

    /// A rendered rate-limit notice never buys a solve.
    ///
    /// The bug: two of the three challenge statuses are also rate-limit statuses, so a 429 from a
    /// Cloudflare-fronted origin reaches the managed-challenge fallback. The solver's invented
    /// 200 then replaces the provider's `Retry-After`, and backoff never fires.
    #[test]
    fn a_rate_limit_notice_is_never_mistaken_for_a_challenge(
        noise in "[a-z <>/]{0,200}",
        title in prop::sample::select(vec![
            "<title>Too Many Requests</title>",
            "<title>429 Too Many Requests</title>",
            "<title>Rate limit exceeded</title>",
        ]),
    ) {
        let body = format!("<html><head>{title}</head><body>{noise}</body></html>");
        // Only meaningful while the body carries no genuine challenge markup; a page that is
        // both is legitimately a challenge.
        prop_assume!(detect_challenge_body(&body).is_none());
        prop_assert!(is_rate_limit_page(&body), "the notice was not recognised");

        for status in [403u16, 429, 503] {
            let resp = Resp {
                status,
                server: Some("cloudflare".to_owned()),
                body: body.clone(),
            };
            prop_assert_eq!(
                detect_challenge(&resp),
                None,
                "a rate-limit notice at {} bought a solve",
                status
            );
        }
    }

    /// An ordinary 200 from a host not behind Cloudflare is classified without reading the
    /// body.
    ///
    /// That is the happy path of the whole crawl, and scanning bodies there would put a substring
    /// search on it.
    #[test]
    fn an_ordinary_response_is_classified_without_consulting_its_body(body in ".*") {
        let resp = Resp { status: 200, server: Some("nginx".to_owned()), body };
        prop_assert_eq!(detect_challenge(&resp), None);
    }
}
