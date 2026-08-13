//! How long a provider that keeps failing is left alone before it is swept again.
//!
//! The sweep is unconditional by design: every active provider, every tick. That is right while
//! providers work and wrong the moment one stops, because nothing else in the pipeline slows
//! down. A provider whose feed has been answering an infrastructure error page for a day was
//! still being asked for it every five minutes — 288 requests a day at a site serving none of
//! them, times the nine installs of the platform that had gone down together. The requests
//! cannot succeed, they fill the failure feed with the same row until nothing else in it is
//! legible, and against a host that is *refusing* rather than broken they are the reason it keeps
//! refusing.
//!
//! So the cadence becomes a function of the provider's own recent history. Pure and separate from
//! the scheduler because the policy is the part worth pinning: what it costs to be wrong in
//! either direction is measured in requests to somebody else's server.

use std::time::Duration;
use tankovault_db::repo::scans::FailureStreak;
use time::OffsetDateTime;

/// Consecutive failures tolerated at the normal cadence before the cooldown starts growing.
///
/// One failure is noise — a page that timed out, a solve that lost its race — and delaying the
/// next sweep for it would make the scanner slower at exactly the moment it needs to confirm
/// whether anything is actually wrong. Two in a row is a pattern.
const FAILURES_BEFORE_BACKOFF: i64 = 2;

/// The backoff policy for one scan mode.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ScanBackoff {
    /// The mode's ordinary sweep interval — the unit the cooldown is expressed in, so the first
    /// step is "skip one sweep" rather than an unrelated constant.
    pub(crate) interval: Duration,
    /// Ceiling on the cooldown. Bounds how long a provider that has recovered stays unscanned,
    /// which is the cost of being wrong in the other direction.
    pub(crate) max: Duration,
}

impl ScanBackoff {
    /// How long `streak` earns before this provider is swept again.
    ///
    /// Doubles per failure past the tolerance and then stops, so a provider that is simply down
    /// settles at one attempt per [`Self::max`] instead of one per interval — enough to notice it
    /// coming back, few enough to be a rounding error in its logs.
    fn cooldown(self, failures: i64) -> Duration {
        let over = failures.saturating_sub(FAILURES_BEFORE_BACKOFF);
        if over < 0 {
            return Duration::ZERO;
        }
        let steps = u32::try_from(over).unwrap_or(u32::MAX);
        self.interval
            .saturating_mul(2u32.saturating_pow(steps.min(31)))
            .min(self.max)
    }

    /// How much of the cooldown is left at `now`, or `None` if the provider is due.
    ///
    /// `None` for an empty streak and for a `max` of zero, which is how an operator switches the
    /// policy off: the sweep then behaves exactly as it did before it existed.
    pub(crate) fn remaining(self, streak: FailureStreak, now: OffsetDateTime) -> Option<Duration> {
        if self.max.is_zero() {
            return None;
        }
        let last_failed_at = streak.last_failed_at?;
        let cooldown = self.cooldown(streak.failures);
        if cooldown.is_zero() {
            return None;
        }
        // A clock that moved backwards (an NTP step) would otherwise read as an enormous
        // remaining wait and park the provider until `max` had passed twice over.
        let elapsed = (now - last_failed_at).try_into().unwrap_or(Duration::ZERO);
        cooldown.checked_sub(elapsed).filter(|d| !d.is_zero())
    }
}

#[cfg(test)]
mod tests {
    use super::{FAILURES_BEFORE_BACKOFF, ScanBackoff};
    use std::time::Duration;
    use tankovault_db::repo::scans::FailureStreak;
    use time::OffsetDateTime;

    const POLICY: ScanBackoff = ScanBackoff {
        interval: Duration::from_secs(300),
        max: Duration::from_secs(21_600),
    };

    fn streak(failures: i64, ago: Duration) -> FailureStreak {
        FailureStreak {
            failures,
            last_failed_at: Some(
                OffsetDateTime::now_utc() - time::Duration::try_from(ago).expect("a small span"),
            ),
        }
    }

    /// A provider that is working, and one that has failed once, are swept at the normal cadence.
    /// Backing off on the first failure would slow the scanner down exactly when it needs to find
    /// out whether the failure repeats.
    #[test]
    fn a_healthy_provider_is_never_held_back() {
        let now = OffsetDateTime::now_utc();
        assert_eq!(
            POLICY.remaining(
                FailureStreak {
                    failures: 0,
                    last_failed_at: None
                },
                now
            ),
            None
        );
        assert_eq!(POLICY.remaining(streak(1, Duration::ZERO), now), None);
    }

    /// The shape of the growth, asserted as exact values rather than as "it increases": the whole
    /// point is *how fast* it stops asking, and an ordering assertion holds for a policy that
    /// takes a week to back off as well as for this one.
    #[test]
    fn the_cooldown_doubles_per_failure_and_then_stops() {
        assert_eq!(POLICY.cooldown(FAILURES_BEFORE_BACKOFF), POLICY.interval);
        assert_eq!(POLICY.cooldown(3), Duration::from_secs(600));
        assert_eq!(POLICY.cooldown(4), Duration::from_secs(1_200));
        assert_eq!(POLICY.cooldown(8), Duration::from_secs(19_200));
        assert_eq!(POLICY.cooldown(9), POLICY.max, "the ceiling holds");
        assert_eq!(POLICY.cooldown(i64::MAX), POLICY.max, "and cannot overflow");
    }

    /// The cooldown is measured from the last failure, so waiting it out is what releases the
    /// provider — not the next failure, which would make the backoff self-perpetuating.
    #[test]
    fn a_provider_is_released_once_the_cooldown_has_elapsed() {
        let now = OffsetDateTime::now_utc();
        assert!(
            POLICY
                .remaining(streak(4, Duration::from_secs(60)), now)
                .is_some()
        );
        assert_eq!(
            POLICY.remaining(streak(4, Duration::from_secs(1_500)), now),
            None
        );
    }

    /// The escape hatch has to be complete: a zero ceiling restores the unconditional sweep
    /// exactly, including for a provider deep into a streak.
    #[test]
    fn a_zero_ceiling_switches_the_policy_off() {
        let off = ScanBackoff {
            max: Duration::ZERO,
            ..POLICY
        };
        assert_eq!(
            off.remaining(streak(50, Duration::ZERO), OffsetDateTime::now_utc()),
            None
        );
    }

    /// A clock that steps backwards must not park a provider for longer than the cooldown. The
    /// subtraction is unsigned, so the naive form underflows into an enormous wait.
    #[test]
    fn a_backwards_clock_does_not_extend_the_wait() {
        let now = OffsetDateTime::now_utc();
        let future = FailureStreak {
            failures: 4,
            last_failed_at: Some(now + time::Duration::hours(1)),
        };
        assert_eq!(POLICY.remaining(future, now), Some(POLICY.cooldown(4)));
    }
}
