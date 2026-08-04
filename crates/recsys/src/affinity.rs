//! How much a reader cares about a series, as one number in `[-1, 1]`.
//!
//! Everything here is derived from signals the product already collects — watchlist status,
//! reading depth, when either last moved. There is no rating column and this design does not add
//! one: implicit feedback is sufficient and does not require asking users to do work.

use tankovault_domain::WatchStatus;

/// The reading depth at which a reader is treated as fully committed.
///
/// Depth is measured in *absolute chapters*, not as a fraction of the series. A fraction punishes
/// the reader at chapter 300 of a 900-chapter ongoing for being two thirds from an end that does
/// not exist yet, which is the opposite of what the data means. Past this knee more chapters add
/// nothing to the *classification* — someone who has cleared sixty chapters has committed.
pub const ENGAGEMENT_KNEE: f32 = 60.0;

/// Half-life of the recency decay, in days.
pub const RECENCY_HALF_LIFE_DAYS: f32 = 180.0;

/// The floor the recency decay never falls through.
///
/// An unfloored decay collapses a dormant reader's profile to noise. An all-time favourite read
/// five years ago is still evidence about taste; it is simply weaker evidence than last week.
pub const RECENCY_FLOOR: f32 = 0.30;

/// What a reader has done with a series.
#[derive(Debug, Clone, Copy)]
pub struct Interaction {
    pub status: WatchStatus,
    /// Whole chapters below the reader's progress marker.
    pub chapters_read: i64,
    /// Days since the watchlist entry or the progress marker last moved, whichever is newer.
    pub age_days: f32,
}

/// Reading depth on `[0, 1]`, log-scaled.
///
/// Logarithmic because the difference between chapter 3 and chapter 30 says far more about
/// whether someone is invested than the difference between chapter 300 and chapter 330.
#[must_use]
pub fn engagement(chapters_read: i64) -> f32 {
    if chapters_read <= 0 {
        return 0.0;
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "chapter counts are far below f32's exact-integer range at any plausible length"
    )]
    let read = chapters_read as f32;
    (read + 1.0).ln() / (ENGAGEMENT_KNEE + 1.0).ln()
}

/// Exponential decay with a floor, so old favourites still count for something.
#[must_use]
pub fn recency(age_days: f32) -> f32 {
    let decayed = 0.5_f32.powf(age_days.max(0.0) / RECENCY_HALF_LIFE_DAYS);
    decayed.max(RECENCY_FLOOR)
}

/// The reader's affinity for one series.
///
/// # `dropped` is not one signal
///
/// Dropped at chapter 3 means "wrong for me". Dropped at chapter 150 means "I liked this for a
/// long time and then it declined" — which is a *positive* statement about everything except the
/// ending. Treating both as `-1` is the classic mistake here, and it actively poisons the profile
/// of anyone who reads long series, because the long series they gave up on are exactly the ones
/// they read most of.
///
/// So `dropped` interpolates: strongly negative when abandoned early, barely negative when
/// abandoned deep.
///
/// # `planned` is intent, not taste
///
/// A plan-to-read list is aspirational and full of things people never open. It must not outweigh
/// a series someone actually read two hundred chapters of, so it sits below every status that
/// implies contact with the work.
#[must_use]
pub fn affinity(interaction: Interaction) -> f32 {
    let depth = engagement(interaction.chapters_read).clamp(0.0, 1.0);
    let decay = recency(interaction.age_days);

    let base = match interaction.status {
        WatchStatus::Completed => 1.0,
        // Scaled by depth: someone two chapters into a series is telling you much less than
        // someone two hundred chapters in, and both are "reading".
        WatchStatus::Reading => 0.80 * depth.mul_add(0.6, 0.4),
        WatchStatus::Paused => 0.35 * depth.mul_add(0.6, 0.4),
        WatchStatus::Planned => 0.25,
        WatchStatus::Dropped => depth.mul_add(0.50, -0.60),
    };

    (base * decay).clamp(-1.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(status: WatchStatus, chapters_read: i64) -> Interaction {
        Interaction {
            status,
            chapters_read,
            age_days: 0.0,
        }
    }

    /// **Dropping something after 150 chapters is not the same signal as dropping it after 3.**
    ///
    /// The bug this pins: a flat `dropped = -1` poisons the profile of anyone who reads long
    /// series, because the long series they abandoned are the ones they read most of — so their
    /// strongest negative signal ends up pointing at exactly what they like.
    #[test]
    fn dropping_deep_is_a_far_weaker_negative_than_dropping_early() {
        let early = affinity(at(WatchStatus::Dropped, 3));
        let deep = affinity(at(WatchStatus::Dropped, 150));
        assert!(early < 0.0, "abandoning early is negative, got {early}");
        assert!(deep < 0.0, "abandoning is still negative, got {deep}");
        assert!(
            deep > early,
            "deep ({deep}) must be a weaker negative than early ({early})"
        );
        assert!(
            deep > -0.2,
            "150 chapters read is nearly neutral, got {deep}"
        );
    }

    /// Intent must not outweigh contact with the work.
    #[test]
    fn a_plan_to_read_counts_for_less_than_anything_actually_read() {
        let planned = affinity(at(WatchStatus::Planned, 0));
        assert!(planned > 0.0);
        assert!(planned < affinity(at(WatchStatus::Reading, 20)));
        assert!(planned < affinity(at(WatchStatus::Completed, 20)));
        assert!(planned < affinity(at(WatchStatus::Paused, 40)));
    }

    #[test]
    fn finishing_is_the_strongest_positive() {
        let completed = affinity(at(WatchStatus::Completed, 200));
        for status in [
            WatchStatus::Reading,
            WatchStatus::Paused,
            WatchStatus::Planned,
            WatchStatus::Dropped,
        ] {
            assert!(
                completed > affinity(at(status, 200)),
                "completed must beat {status:?}"
            );
        }
    }

    /// Depth is absolute, not a fraction: this is what stops a reader 300 chapters into a
    /// 900-chapter ongoing from being scored as less invested than one who finished a oneshot.
    #[test]
    fn reading_deeper_raises_affinity() {
        let shallow = affinity(at(WatchStatus::Reading, 2));
        let deep = affinity(at(WatchStatus::Reading, 200));
        assert!(deep > shallow, "{deep} should exceed {shallow}");
    }

    #[test]
    fn engagement_saturates_rather_than_growing_without_bound() {
        assert!(engagement(0).abs() < f32::EPSILON);
        assert!(
            engagement(-5).abs() < f32::EPSILON,
            "a negative count is not negative depth"
        );
        assert!(engagement(60) > 0.99 && engagement(60) < 1.01);
        // Past the knee, more chapters barely move it.
        assert!((engagement(5_000) - engagement(1_000)).abs() < 0.6);
    }

    /// An old favourite is weaker evidence, never no evidence.
    #[test]
    fn recency_decays_to_a_floor_and_not_to_zero() {
        assert!((recency(0.0) - 1.0).abs() < 1e-6);
        assert!(recency(180.0) < recency(0.0));
        assert!(recency(180.0) > recency(720.0));
        assert!(
            recency(10_000.0) >= RECENCY_FLOOR,
            "decay must not fall through the floor"
        );
    }

    /// Recency scales, it does not invert: a stale positive stays positive and a stale negative
    /// stays negative.
    #[test]
    fn decay_never_changes_a_signals_sign() {
        for age in [0.0, 90.0, 400.0, 5_000.0] {
            let liked = affinity(Interaction {
                status: WatchStatus::Completed,
                chapters_read: 50,
                age_days: age,
            });
            let hated = affinity(Interaction {
                status: WatchStatus::Dropped,
                chapters_read: 1,
                age_days: age,
            });
            assert!(liked > 0.0, "a favourite stays positive at {age} days");
            assert!(hated < 0.0, "a rejection stays negative at {age} days");
        }
    }

    #[test]
    fn affinity_stays_inside_its_range() {
        for status in WatchStatus::all() {
            for chapters in [0, 1, 60, 10_000] {
                for age in [0.0, 1_000.0] {
                    let value = affinity(Interaction {
                        status: *status,
                        chapters_read: chapters,
                        age_days: age,
                    });
                    assert!(
                        (-1.0..=1.0).contains(&value),
                        "{status:?}/{chapters}/{age} produced {value}"
                    );
                }
            }
        }
    }
}
