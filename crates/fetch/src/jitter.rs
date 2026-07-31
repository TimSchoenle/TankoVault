//! Full-jitter exponential backoff: the delay policy both retrying fetch layers use.
//!
//! # Why this is a module rather than a method on each layer
//!
//! It was a private `fn backoff(&self, attempt: u32)` on **both** [`crate::backoff::BackoffFetcher`]
//! and [`crate::retry::RetryingFetcher`], with identical bodies down to the comment. Nothing
//! connected them, which is the shape of duplication this codebase has already had to un-drift
//! three times (`repo_browse`, `repo_tracking`/TRACK-1, the browse `WHERE` clause). Here the two
//! genuinely are one policy — the layers differ in *what* they retry, not in how long they wait.
//!
//! # Why the RNG is a parameter (TEST F-09)
//!
//! The delay was drawn from the thread-local generator (`rand::rng()`) inline, so no test could
//! assert anything about it beyond "it did not panic". That matters more than it sounds: the
//! whole point of full jitter is the *spread*, and a mutant replacing the draw with the constant
//! `0` — or with the ceiling — survives every bound-only assertion while destroying the property.
//! That is precisely the class of surviving mutant F-10 spent a session eliminating.
//!
//! So [`ceiling`] is pure and exactly assertable, and [`full_jitter`] takes the generator. The
//! shape follows the convention this remediation settled on for the rate limiter's clock: pass
//! the source of non-determinism as an argument rather than building a service abstraction over
//! it, and give production a wrapper so no call site has to know.

// `rand` 0.10 split the old `Rng` in two: `Rng` is the core generator trait (what a caller can
// hand us), `RngExt` is the blanket-implemented sampling surface `random_range` lives on. The
// bound stays on `Rng` so callers only have to satisfy the core trait.
use rand::{Rng, RngExt as _};
use std::time::Duration;

/// The upper bound of the delay for `attempt` (1-based): `base * 2^(attempt - 1)`, capped at
/// `max`.
///
/// Pure, total, and saturating at every step — a large `attempt` saturates the exponent, the
/// multiply saturates, and the `min` caps the result, so there is no input that panics or wraps.
/// `attempt = 0` is treated as `attempt = 1` rather than rejected, because a caller counting from
/// zero should get the shortest wait rather than a surprise.
pub(crate) fn ceiling(base: Duration, max: Duration, attempt: u32) -> Duration {
    base.saturating_mul(2u32.saturating_pow(attempt.saturating_sub(1)))
        .min(max)
}

/// A uniform draw from `[0, ceiling(base, max, attempt)]` — "full jitter".
///
/// Spreading a fleet's retries over the whole interval rather than clustering them at the
/// ceiling is the point: synchronised retries are a second thundering herd against a host that
/// is already struggling, which is what earned the `429` in the first place.
pub(crate) fn full_jitter<R: Rng>(
    rng: &mut R,
    base: Duration,
    max: Duration,
    attempt: u32,
) -> Duration {
    let millis = u64::try_from(ceiling(base, max, attempt).as_millis()).unwrap_or(u64::MAX);
    Duration::from_millis(rng.random_range(0..=millis))
}

/// [`full_jitter`] against the thread-local generator — the production entry point.
pub(crate) fn full_jitter_now(base: Duration, max: Duration, attempt: u32) -> Duration {
    full_jitter(&mut rand::rng(), base, max, attempt)
}

#[cfg(test)]
mod tests {
    use super::{ceiling, full_jitter};
    use rand::SeedableRng as _;
    use rand::rngs::StdRng;
    use std::time::Duration;

    const BASE: Duration = Duration::from_millis(100);
    const MAX: Duration = Duration::from_secs(30);

    /// The ceiling doubles per attempt and then stops at `max`.
    ///
    /// Asserted as exact values rather than as an ordering, deliberately: an ordering assertion
    /// is what let five mutants survive in the matcher (F-10), because "grows" and "grows by the
    /// right factor" are different claims and only the second one is the policy.
    #[test]
    fn the_ceiling_doubles_until_it_reaches_the_cap() {
        assert_eq!(ceiling(BASE, MAX, 1), Duration::from_millis(100));
        assert_eq!(ceiling(BASE, MAX, 2), Duration::from_millis(200));
        assert_eq!(ceiling(BASE, MAX, 3), Duration::from_millis(400));
        assert_eq!(ceiling(BASE, MAX, 9), Duration::from_millis(25_600));
        // 100ms * 2^9 = 51.2s, past the cap.
        assert_eq!(ceiling(BASE, MAX, 10), MAX);
    }

    /// Attempt `0` is the shortest wait, not an overflow.
    ///
    /// `attempt - 1` on a `u32` zero is the one input that would panic in debug and wrap in
    /// release, and SEC-11 turned `overflow-checks` on in release too — so this is the arithmetic
    /// that has to saturate rather than the arithmetic that merely should.
    #[test]
    fn attempt_zero_is_treated_as_the_first_attempt() {
        assert_eq!(ceiling(BASE, MAX, 0), ceiling(BASE, MAX, 1));
    }

    /// A caller that keeps counting cannot make this panic or wrap.
    #[test]
    fn an_absurd_attempt_number_saturates_at_the_cap() {
        assert_eq!(ceiling(BASE, MAX, u32::MAX), MAX);
        assert_eq!(ceiling(Duration::MAX, MAX, u32::MAX), MAX);
    }

    /// Every draw lands inside the ceiling.
    #[test]
    fn every_draw_is_within_the_ceiling() {
        let mut rng = StdRng::seed_from_u64(1);
        for attempt in 0..12 {
            let bound = ceiling(BASE, MAX, attempt);
            for _ in 0..64 {
                assert!(full_jitter(&mut rng, BASE, MAX, attempt) <= bound);
            }
        }
    }

    /// **The claim the bound alone cannot make**: the draw spans the whole interval.
    ///
    /// Without this, `random_range(0..=millis)` could be replaced by the constant `0`, by the
    /// constant `millis`, or by a draw over a tenth of the range, and every other test here would
    /// still pass — while full jitter, whose entire purpose is the spread, would be gone. Stated
    /// as "both halves of the interval are reached" rather than as a distribution test, because
    /// that is the weakest claim that still fails for all three of those mutants.
    #[test]
    fn the_draw_reaches_both_halves_of_the_interval() {
        let mut rng = StdRng::seed_from_u64(7);
        let bound = ceiling(BASE, MAX, 5);
        let midpoint = bound / 2;
        let mut below = false;
        let mut above = false;
        for _ in 0..256 {
            let delay = full_jitter(&mut rng, BASE, MAX, 5);
            below |= delay < midpoint;
            above |= delay > midpoint;
        }
        assert!(below, "no draw fell in the lower half of [0, ceiling]");
        assert!(above, "no draw fell in the upper half of [0, ceiling]");
    }

    /// The seed is what makes the assertion above safe to keep.
    ///
    /// A `thread_rng` version of that test is a coin flip repeated 256 times — overwhelmingly
    /// likely to pass, and therefore a flake nobody can reproduce when it does not. Pinning the
    /// generator makes the run deterministic, which is the axis TEST F-09 asked for; the failing
    /// case here would be a *code* change, never a scheduling accident.
    #[test]
    fn the_same_seed_produces_the_same_sequence() {
        let draws = |seed| {
            let mut rng = StdRng::seed_from_u64(seed);
            (0..8)
                .map(|attempt| full_jitter(&mut rng, BASE, MAX, attempt))
                .collect::<Vec<_>>()
        };
        assert_eq!(draws(42), draws(42));
        assert_ne!(draws(42), draws(43), "different seeds must not agree");
    }
}
