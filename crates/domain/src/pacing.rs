//! Outbound request pacing: a minimum gap plus an adaptive penalty for provider push-back.
//!
//! # Why this lives in `domain`
//!
//! Politeness policy is the thing most likely to get a deployment blocked, and it used to be
//! implemented three times at three different capability levels (ARCH-20): the crawler's
//! `Throttle` (governor-backed, with a decaying 429 penalty), the crawler's `BackoffFetcher`
//! (`Retry-After` aware), and a private minimum-gap mutex in the `AniList` client. The last had no
//! persistent penalty at all — it retried a `429` once and then went straight back to full rate,
//! which is the behaviour a provider reads as ignoring them.
//!
//! It is here rather than in `tankovault-fetch` for the same reason [`crate::ssrf`] is: a consumer
//! must be able to pace its own outbound calls without pulling in the whole wreq/BoringSSL crawl
//! stack. `services/sync` talks to `AniList` over plain `reqwest` and has no business linking a
//! browser-emulating TLS stack to do it.
//!
//! # Transport-agnostic by construction
//!
//! [`Pacer`] never sleeps and never touches a clock of its own: [`Pacer::reserve`] takes `now`,
//! claims the next slot, and returns how long the caller must wait. The caller does the sleeping
//! with whatever runtime it has.
//!
//! Reserving rather than holding a lock across the wait matters: the `AniList` client's private
//! version held its mutex for the whole sleep, so N concurrent callers serialised into a queue N
//! gaps long instead of each taking the next free slot.

use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

/// How pacing reacts to a provider answering "too many requests".
///
/// Additive-then-multiplicative on the way up, halving on the way down: a caller that trips a
/// limit backs well off it quickly, and returns to full speed slowly enough not to trip it again
/// on the way.
#[derive(Debug, Clone, Copy)]
pub struct PacingPolicy {
    /// Spacing added by the first throttle signal, and the floor for any non-zero penalty.
    pub step: Duration,
    /// Ceiling on the added spacing, so a provider that answers `429` unconditionally (or a
    /// misread `Retry-After`) cannot park a worker indefinitely.
    pub max: Duration,
    /// Throttle-free time after which the penalty halves.
    pub recovery: Duration,
}

impl Default for PacingPolicy {
    fn default() -> Self {
        Self {
            // A half-second of extra spacing is a large correction at crawl rates of a few rps,
            // and small enough that a single stray 429 costs almost nothing.
            step: Duration::from_millis(500),
            // 8s spacing is ~0.125 rps: slow, but still progressing. Past this the provider is
            // not rate-limiting us, it is refusing us, and that is the backoff layer's and the
            // scheduler's problem, not the pacer's.
            max: Duration::from_secs(8),
            recovery: Duration::from_secs(60),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct State {
    /// Earliest instant the next reserved request may go out. `None` before the first.
    next_allowed: Option<Instant>,
    penalty: Duration,
    /// When the penalty may next halve. Meaningless while `penalty` is zero.
    next_decay: Instant,
}

/// A minimum gap between outbound requests, widened while a provider is pushing back.
pub struct Pacer {
    min_interval: Duration,
    policy: PacingPolicy,
    state: Mutex<State>,
}

impl Pacer {
    /// A pacer enforcing at least `min_interval` between requests, adapting under `policy`.
    ///
    /// A zero `min_interval` is legal and means "no floor of our own" — the penalty still
    /// applies, which is the configuration [`crate::pacing`]'s crawler consumer uses because its
    /// floor comes from the provider's configured crawl delay instead.
    #[must_use]
    pub fn new(min_interval: Duration, policy: PacingPolicy) -> Self {
        Self {
            min_interval,
            policy,
            state: Mutex::new(State {
                next_allowed: None,
                penalty: Duration::ZERO,
                next_decay: Instant::now(),
            }),
        }
    }

    /// Lock the state.
    ///
    /// Poisoning is recovered from rather than propagated: the state is three plain values with
    /// no invariant a panicking thread could have broken, and refusing to make requests for the
    /// rest of a process' life is a far worse failure than resuming with a stale penalty.
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Decay the penalty if the provider has been quiet for a full recovery window, and return
    /// the current extra spacing.
    ///
    /// Use this where the caller already has its own spacing floor and wants the wider of the
    /// two; use [`Self::reserve`] where the pacer owns the schedule.
    pub fn penalty(&self, now: Instant) -> Duration {
        let mut state = self.state();
        Self::decay(&mut state, self.policy, now);
        state.penalty
    }

    /// Claim the next slot and return how long the caller must wait before using it.
    ///
    /// The slot is claimed whether or not the caller actually waits, so two concurrent callers
    /// get two consecutive slots rather than the same one.
    pub fn reserve(&self, now: Instant) -> Duration {
        let mut state = self.state();
        Self::decay(&mut state, self.policy, now);
        let gap = self.min_interval + state.penalty;

        let earliest = match state.next_allowed {
            // A slot in the past means the pacer has been idle; start the schedule from now
            // rather than letting unused slots accumulate into a burst allowance.
            Some(at) if at > now => at,
            _ => now,
        };
        state.next_allowed = Some(earliest + gap);
        earliest.saturating_duration_since(now)
    }

    /// Record a throttle signal, widening the spacing.
    ///
    /// `retry_after` is the provider's own instruction when it sent a usable one. It is honoured
    /// as a floor rather than taken verbatim: a provider asking for less than the penalty we have
    /// already accumulated is not evidence that the earlier signals were wrong. It is still
    /// clamped to [`PacingPolicy::max`], because a hostile or mistyped header must not be able to
    /// park a worker for hours.
    pub fn penalise(&self, now: Instant, retry_after: Option<Duration>) {
        let mut state = self.state();
        let grown = if state.penalty.is_zero() {
            self.policy.step
        } else {
            state.penalty * 2
        };
        state.penalty = grown
            .max(retry_after.unwrap_or(Duration::ZERO))
            .min(self.policy.max);
        state.next_decay = now + self.policy.recovery;
    }

    fn decay(state: &mut State, policy: PacingPolicy, now: Instant) {
        if !state.penalty.is_zero() && now >= state.next_decay {
            let halved = state.penalty / 2;
            state.penalty = if halved < policy.step {
                Duration::ZERO
            } else {
                halved
            };
            state.next_decay = now + policy.recovery;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> PacingPolicy {
        PacingPolicy {
            step: Duration::from_millis(500),
            max: Duration::from_secs(8),
            recovery: Duration::from_secs(60),
        }
    }

    #[test]
    fn the_first_request_waits_for_nothing() {
        let pacer = Pacer::new(Duration::from_secs(1), policy());
        assert_eq!(pacer.reserve(Instant::now()), Duration::ZERO);
    }

    /// Two callers arriving at the same instant must get *consecutive* slots. The `AniList`
    /// client's private pacer held its mutex across the sleep to achieve this, which serialised
    /// every caller behind the whole queue; reserving does it without holding anything.
    #[test]
    fn concurrent_callers_get_consecutive_slots() {
        let pacer = Pacer::new(Duration::from_secs(1), policy());
        let now = Instant::now();
        assert_eq!(pacer.reserve(now), Duration::ZERO);
        assert_eq!(pacer.reserve(now), Duration::from_secs(1));
        assert_eq!(pacer.reserve(now), Duration::from_secs(2));
    }

    /// An idle pacer must not bank unused slots into a burst: the point is a *gap* between
    /// requests, not an average rate.
    #[test]
    fn idle_time_is_not_banked_as_a_burst_allowance() {
        let pacer = Pacer::new(Duration::from_secs(1), policy());
        let start = Instant::now();
        assert_eq!(pacer.reserve(start), Duration::ZERO);
        let much_later = start + Duration::from_secs(60);
        assert_eq!(pacer.reserve(much_later), Duration::ZERO);
        assert_eq!(pacer.reserve(much_later), Duration::from_secs(1));
    }

    /// The gap the `AniList` client was missing entirely: after a `429`, later requests must stay
    /// spaced out. Its private pacer retried once and then went back to full rate.
    #[test]
    fn a_throttle_signal_widens_every_later_gap() {
        let pacer = Pacer::new(Duration::from_secs(1), policy());
        let now = Instant::now();
        pacer.penalise(now, None);
        assert_eq!(pacer.reserve(now), Duration::ZERO);
        assert_eq!(
            pacer.reserve(now),
            Duration::from_millis(1500),
            "the gap is the minimum interval plus the penalty"
        );
    }

    #[test]
    fn repeated_signals_double_the_penalty_up_to_the_ceiling() {
        let pacer = Pacer::new(Duration::ZERO, policy());
        let now = Instant::now();
        for _ in 0..20 {
            pacer.penalise(now, None);
        }
        assert_eq!(pacer.penalty(now), Duration::from_secs(8));
    }

    /// `Retry-After` is a floor, not a replacement: a provider asking for 50 ms after we have
    /// already accumulated seconds of penalty is not evidence the earlier signals were wrong.
    #[test]
    fn retry_after_raises_the_penalty_but_never_lowers_it() {
        let pacer = Pacer::new(Duration::ZERO, policy());
        let now = Instant::now();
        pacer.penalise(now, Some(Duration::from_secs(3)));
        assert_eq!(pacer.penalty(now), Duration::from_secs(3));
        pacer.penalise(now, Some(Duration::from_millis(50)));
        assert_eq!(
            pacer.penalty(now),
            Duration::from_secs(6),
            "a smaller Retry-After must not undo the doubling"
        );
    }

    /// A hostile or mistyped header must not be able to park a worker for hours.
    #[test]
    fn an_absurd_retry_after_is_clamped() {
        let pacer = Pacer::new(Duration::ZERO, policy());
        let now = Instant::now();
        pacer.penalise(now, Some(Duration::from_secs(86_400)));
        assert_eq!(pacer.penalty(now), Duration::from_secs(8));
    }

    #[test]
    fn the_penalty_halves_after_a_quiet_recovery_window_and_reaches_zero() {
        let pacer = Pacer::new(Duration::ZERO, policy());
        let start = Instant::now();
        pacer.penalise(start, None); // 500ms
        pacer.penalise(start, None); // 1s
        assert_eq!(pacer.penalty(start), Duration::from_secs(1));

        let after_one = start + Duration::from_secs(61);
        assert_eq!(pacer.penalty(after_one), Duration::from_millis(500));
        // Halving 500ms yields less than `step`, which means "recovered" rather than a floor of
        // 250ms that never clears.
        let after_two = after_one + Duration::from_secs(61);
        assert_eq!(pacer.penalty(after_two), Duration::ZERO);
    }
}
