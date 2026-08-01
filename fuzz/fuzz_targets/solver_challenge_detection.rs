//! Fuzzes `tankovault_solver`'s in-band challenge classifier, which runs on every response the
//! crawler receives and is entirely provider/solver-controlled (truncated bodies, binary
//! payloads decoded as UTF-8, rendered interstitials).
//!
//! # Oracle
//! (1) is shared with the stable properties in `crates/solver/tests/prop_detection.rs`:
//! 1. No panic, over arbitrary UTF-8 of arbitrary length.
//! 2. The narrow classifier (`detect_challenge_body`) never accepts what the broad one
//!    (`detect_challenge`) rejects — disagreement here means an inscrutable parse failure
//!    downstream instead of "still challenged".
//! 3. A rate-limit notice never buys a solve: two of the three challenge statuses are also
//!    rate-limit statuses, which is how this regressed once — a `429` fell through to the
//!    solver fallback and the backoff layer never fired.
//!
//! # Why fuzz when the property tests already assert (2) and (3)
//! Reach: `prop_detection.rs`'s `".*"` strategy tops out around 32 characters, too short to
//! exercise the `TITLE_SCAN_BYTES` cut, and its marker bodies are built from a fixed fragment
//! list, so it can only find disagreements assembled from fragments already considered. A
//! coverage-guided mutator hits neither limit, against the same assertions.

#![no_main]

use libfuzzer_sys::fuzz_target;
use tankovault_solver::{
    ResponseView, detect_challenge, detect_challenge_body, is_rate_limit_page,
};

/// The minimal envelope the classifier needs, under our control.
///
/// `403` + `server: cloudflare` is the most permissive envelope: it satisfies every early
/// return in `detect_challenge`, so the body is guaranteed to be read and the differential
/// below isn't vacuous.
struct Resp<'a> {
    status: u16,
    body: &'a str,
}

impl ResponseView for Resp<'_> {
    fn status(&self) -> u16 {
        self.status
    }

    fn header(&self, name: &str) -> Option<&str> {
        name.eq_ignore_ascii_case("server").then_some("cloudflare")
    }

    fn body_snippet(&self) -> &str {
        self.body
    }
}

fuzz_target!(|data: &str| {
    let body_kind = detect_challenge_body(data);
    let throttled = is_rate_limit_page(data);

    // (2) Whatever the narrow classifier accepts, the broad one accepts too — by construction,
    // since `detect_challenge` consults `detect_challenge_body` first. Moving the rate-limit
    // check above it would break this for a body that is both.
    if body_kind.is_some() {
        assert!(
            detect_challenge(&Resp {
                status: 403,
                body: data
            })
            .is_some(),
            "the narrow classifier accepted a body the broad one calls clean"
        );
    }

    // (3) A throttle notice is the origin answering, not an interstitial in front of it.
    // Guarded on no genuine challenge markup: a body that is both is legitimately a challenge.
    if throttled && body_kind.is_none() {
        for status in [403u16, 429, 503] {
            assert_eq!(
                detect_challenge(&Resp { status, body: data }),
                None,
                "a rate-limit notice bought a solve at {status}"
            );
        }
    }
});
