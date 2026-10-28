//! Owns full-jitter exponential backoff, the delay policy [`crate::backoff`] and
//! [`crate::retry`] share: the two differ in what they retry, not in how long they wait.
//!
//! [`ceiling`] is pure and exactly assertable. [`full_jitter`] takes the RNG as a parameter, so
//! a test can catch a draw replaced by a constant. [`full_jitter_now`] wraps it for production
//! callers.

// `Rng` is the core generator trait a caller provides; `RngExt` is the blanket sampling surface
// `random_range` lives on. The bound stays on `Rng` so callers satisfy only the core trait.
use rand::{Rng, RngExt as _};
use std::time::Duration;

/// Returns the delay ceiling for the 1-based `attempt`: `base * 2^(attempt - 1)`, capped at
/// `max`.
///
/// Pure, total and saturating at every step, so no input panics or wraps. `attempt = 0` is
/// treated as `1`, so a caller counting from zero gets the shortest wait.
pub(crate) fn ceiling(base: Duration, max: Duration, attempt: u32) -> Duration {
    base.saturating_mul(2u32.saturating_pow(attempt.saturating_sub(1)))
        .min(max)
}

/// Draws uniformly from `[0, ceiling(base, max, attempt)]`, "full jitter".
///
/// Spreading a fleet's retries over the whole interval is the point: retries clustered at the
/// ceiling are a second thundering herd against a host that is already struggling.
pub(crate) fn full_jitter<R: Rng>(
    rng: &mut R,
    base: Duration,
    max: Duration,
    attempt: u32,
) -> Duration {
    let millis = u64::try_from(ceiling(base, max, attempt).as_millis()).unwrap_or(u64::MAX);
    Duration::from_millis(rng.random_range(0..=millis))
}

/// Runs [`full_jitter`] against the thread-local generator, for production callers.
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
    /// Asserted as exact values, not an ordering: "grows" and "grows by the right factor" are
    /// different claims, and only the second is the policy.
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
    /// `attempt - 1` on a `u32` zero is the one input that would overflow, and `overflow-checks`
    /// is on in release too, so it must saturate.
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

    /// **The draw spans the whole interval**, which the bound alone cannot show.
    ///
    /// Without this, the draw could become the constant `0`, the constant `millis` or a draw over
    /// a tenth of the range, and every other test would pass. "Both halves are reached" is the
    /// weakest claim that still fails for all three.
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

    /// The same seed produces the same sequence, which keeps the test above deterministic.
    ///
    /// With `thread_rng` it would be a coin flip repeated 256 times: almost always passing, and an
    /// unreproducible flake when not. Seeded, only a code change can fail it.
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
