//! **F-T4** — `tankovault_solver`'s in-band challenge classifier, which runs on **every**
//! response the crawler receives.
//!
//! Its input is entirely provider- or solver-controlled and is frequently not markup at all:
//! a truncated body, a binary payload that decoded as UTF-8, a rendered interstitial from a
//! headless browser. Two things it does are worth coverage-guided attention rather than
//! example tests.
//!
//! `is_rate_limit_page` cuts a 4096-byte prefix by **byte** offset and walks backwards with
//! `is_char_boundary` to avoid splitting a character. That is correct today and it is exactly
//! the shape of the audit's first verified defect (F-01), where a byte offset taken from one
//! string was applied to another. `detect_challenge` then scans that same body for markers
//! whose presence decides whether the crawl pays for a solve.
//!
//! # Oracle
//!
//! Three, and only the first is shared with the stable properties in
//! `crates/solver/tests/prop_detection.rs`:
//!
//! 1. **No panic**, over arbitrary UTF-8 of arbitrary length.
//! 2. **The narrow classifier never accepts what the broad one rejects.** `detect_challenge_body`
//!    is used where there is no status or header envelope to corroborate the markers — chiefly
//!    the HTML a solver hands back. If it could fire on a body `detect_challenge` calls clean,
//!    the fetch stack and the adapter layer would disagree about the same bytes, and the symptom
//!    downstream is an inscrutable parse failure rather than "still challenged".
//! 3. **A rate-limit notice never buys a solve.** Two of the three challenge statuses are also
//!    the rate-limit statuses, which is how this regressed before: every `429` from a
//!    Cloudflare-fronted origin fell through to the managed-challenge fallback, the provider's
//!    `Retry-After` was replaced by a solver-invented `200`, and the backoff layer never fired.
//!
//! # Why this exists when `prop_detection.rs` already asserts (2) and (3)
//!
//! Reach. Proptest's `".*"` expands to roughly 32 characters, so the stable suite cannot
//! generate a body long enough to exercise the `TITLE_SCAN_BYTES` cut at all — it needs a
//! hand-written `"€".repeat(1350)` case to touch it, which pins one length rather than the
//! boundary. It also generates marker-bearing bodies by concatenating a **fixed list** of
//! fragments, so it can only ever find a disagreement assembled from fragments somebody already
//! thought of. A coverage-guided mutator working from the seeds below reaches neither of those
//! limits, and the assertions are the same ones, so a hit here is reproducible there.

#![no_main]

use libfuzzer_sys::fuzz_target;
use tankovault_solver::{
    ResponseView, detect_challenge, detect_challenge_body, is_rate_limit_page,
};

/// The minimal envelope the classifier needs, under our control.
///
/// `403` + `server: cloudflare` is deliberately the most *permissive* envelope there is: it
/// satisfies every early return in `detect_challenge`, so the body is guaranteed to be read.
/// Any other envelope would let the classifier short-circuit before the comparison this target
/// is making, and the differential would pass vacuously.
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

    // (2) Whatever the narrow classifier accepts, the broad one accepts too.
    //
    // This holds by construction today — `detect_challenge` consults `detect_challenge_body`
    // first — and that is the point: the ordering *is* the invariant. Moving the
    // `is_rate_limit_page` check above it would break this for any body that is both, which is
    // a body the mutator can build and a reviewer would not think to.
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
    //
    // Guarded on the body carrying no genuine challenge markup: a page that is both is
    // legitimately a challenge, and the same guard is why `prop_detection.rs` uses a
    // `prop_assume!` here.
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
