//! Properties of the in-band challenge classifier.
//!
//! The classifier runs on **every** response the crawler receives, so its input is entirely
//! provider- (or solver-) controlled and includes bodies that are not markup at all. Two
//! things matter and neither is checked by an example test:
//!
//! 1. It cannot panic. `is_rate_limit_page` walks backwards with `is_char_boundary` to cut a
//!    prefix by *byte* count — correct today, and exactly the shape of the defect this audit
//!    found in `parse_chapter_number`, where a byte offset from one string was applied to
//!    another. A property over arbitrary Unicode is what keeps it correct.
//! 2. The narrow body-only classifier must never accept what the broad envelope-aware one
//!    rejects. `detect_challenge_body` is used where there is no status or header to
//!    corroborate the markers; if it could fire on a body that `detect_challenge` calls clean,
//!    the fetch stack would report "still challenged" for a page the same code considers
//!    ordinary, and the two would disagree about the same bytes.

use proptest::prelude::*;
use tankovault_solver::{
    ResponseView, detect_challenge, detect_challenge_body, is_rate_limit_page,
};

/// The minimal response the classifier needs, under our control.
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

/// Bodies built from the markers the classifier looks for, interleaved with arbitrary text.
///
/// Uniform random strings would essentially never contain `cf-turnstile` or `<title>Too Many
/// Requests`, so a naive `".*"` strategy would only ever exercise the negative path. This
/// generates the fragments that actually decide the outcome.
fn marker_body() -> impl Strategy<Value = String> {
    let fragment = prop::sample::select(vec![
        "cf-turnstile",
        "challenges.cloudflare.com/turnstile",
        "/cdn-cgi/challenge-platform/h/b/orchestrate/chl_page/v1?ray=x",
        // The JS Detections beacon, which rides on ordinary content pages: it shares the
        // interstitial's path prefix and must not decide the outcome on its own.
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

    /// The same, with the cut deliberately landing inside a multi-byte character: a string of
    /// three-byte characters is long enough to be truncated and cannot be cut at 4096 without
    /// the backwards walk doing its job.
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

    /// The differential invariant: whatever the narrow classifier accepts, the broad one
    /// accepts too, given an envelope that lets it look at the body at all.
    ///
    /// If this ever fails, the fetch stack and the adapter layer disagree about the same
    /// bytes — one reporting a challenge the other calls content — and the symptom downstream
    /// is an inscrutable parse failure rather than "still challenged".
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

    /// A rendered rate-limit notice is the origin answering, not an interstitial in front of
    /// it, so it must never buy a solve. Two of the three challenge statuses are also the
    /// rate-limit statuses, which is exactly how this regressed before: every 429 from a
    /// Cloudflare-fronted origin fell through to the managed-challenge fallback, the
    /// provider's `Retry-After` was replaced by a solver-invented 200, and the backoff layer
    /// never fired.
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

    /// An ordinary 200 from a host that is not behind Cloudflare short-circuits before the
    /// body is read at all. That is the happy path for every page the crawler fetches, and a
    /// change that started scanning bodies there would put a substring search on the hot path
    /// of the whole crawl.
    #[test]
    fn an_ordinary_response_is_classified_without_consulting_its_body(body in ".*") {
        let resp = Resp { status: 200, server: Some("nginx".to_owned()), body };
        prop_assert_eq!(detect_challenge(&resp), None);
    }
}
