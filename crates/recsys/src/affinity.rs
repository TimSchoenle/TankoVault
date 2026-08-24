//! How much a reader cares about a series, as one number in `[-1, 1]`.
//!
//! Everything here is derived from signals the product already collects — watchlist status,
//! reading depth, when either last moved. There is no rating column and this design does not add
//! one: implicit feedback is sufficient and does not require asking users to do work.

use tankovault_domain::{Tunable, WatchStatus};

/// The shape of the affinity curve, as an operator has it set.
///
/// Threaded in as a parameter rather than read from a global: these are pure functions, and a
/// process-wide value would make them untestable at any setting but the live one — and would put
/// a lock acquisition inside a loop over a reader's whole watchlist.
///
/// [`Default`] reads the compiled defaults out of [`Tunable`], so there is no second copy of the
/// numbers to drift from the registry the console edits.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AffinityParams {
    /// Affinity for a finished series — the reference the others are read against.
    pub base_completed: f32,
    /// Base for a series currently being read, before depth scaling.
    pub base_reading: f32,
    /// Base for a paused series, before depth scaling.
    pub base_paused: f32,
    /// Base for a plan-to-read entry: intent, not taste, so it takes no depth scaling.
    pub base_planned: f32,
    /// Affinity for a series abandoned immediately. Negative.
    pub dropped_floor: f32,
    /// How far a fully committed reader claws back from [`Self::dropped_floor`].
    pub dropped_span: f32,
    /// Chapters at which a reader counts as fully committed.
    ///
    /// Depth is measured in *absolute chapters*, not as a fraction of the series. A fraction
    /// punishes the reader at chapter 300 of a 900-chapter ongoing for being two thirds from an
    /// end that does not exist yet, which is the opposite of what the data means.
    pub engagement_knee: f32,
    /// Days after which a signal counts half as much.
    pub recency_half_life_days: f32,
    /// The floor the recency decay never falls through.
    ///
    /// An unfloored decay collapses a dormant reader's profile to noise. An all-time favourite
    /// read five years ago is still evidence about taste; it is simply weaker evidence than last
    /// week.
    pub recency_floor: f32,
}

impl Default for AffinityParams {
    fn default() -> Self {
        // Narrowing from the registry's `f64`: every one of these ranges is far inside `f32`.
        #[expect(
            clippy::cast_possible_truncation,
            reason = "affinity ranges are all within [-1, 3650]"
        )]
        let at = |tunable: Tunable| tunable.default_value() as f32;
        Self {
            base_completed: at(Tunable::AffinityBaseCompleted),
            base_reading: at(Tunable::AffinityBaseReading),
            base_paused: at(Tunable::AffinityBasePaused),
            base_planned: at(Tunable::AffinityBasePlanned),
            dropped_floor: at(Tunable::AffinityDroppedFloor),
            dropped_span: at(Tunable::AffinityDroppedSpan),
            engagement_knee: at(Tunable::AffinityEngagementKnee),
            recency_half_life_days: at(Tunable::AffinityRecencyHalfLifeDays),
            recency_floor: at(Tunable::AffinityRecencyFloor),
        }
    }
}

/// What a reader has done with a series.
#[derive(Debug, Clone, Copy)]
pub struct Interaction {
    /// The reader's own label. It picks the base weight, which reading depth then scales for
    /// every status but `Completed` and `Planned`.
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
///
/// The saturation at 1 is part of the contract, not a rounding detail: the knee is *the* point at
/// which a reader counts as fully committed, and `user_series_affinity.engagement` is constrained
/// to `[0, 1]` on that basis. Anything past the knee is still just committed.
#[must_use]
pub fn engagement(chapters_read: i64, knee: f32) -> f32 {
    if chapters_read <= 0 {
        return 0.0;
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "chapter counts are far below f32's exact-integer range at any plausible length"
    )]
    let read = chapters_read as f32;
    // The registry floors the knee at five, but a caller can construct params by hand; a knee at
    // or below zero would divide by a non-positive logarithm and produce a sign flip rather than
    // an error.
    ((read + 1.0).ln() / (knee.max(1.0) + 1.0).ln()).min(1.0)
}

/// Exponential decay with a floor, so old favourites still count for something.
#[must_use]
pub fn recency(age_days: f32, half_life_days: f32, floor: f32) -> f32 {
    let decayed = 0.5_f32.powf(age_days.max(0.0) / half_life_days.max(f32::EPSILON));
    decayed.max(floor)
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
pub fn affinity(interaction: Interaction, params: &AffinityParams) -> f32 {
    let depth = engagement(interaction.chapters_read, params.engagement_knee);
    let decay = recency(
        interaction.age_days,
        params.recency_half_life_days,
        params.recency_floor,
    );

    let base = match interaction.status {
        WatchStatus::Completed => params.base_completed,
        // Scaled by depth: someone two chapters into a series is telling you much less than
        // someone two hundred chapters in, and both are "reading". The 0.4 floor is the share a
        // held series keeps regardless of depth, and is deliberately not a knob — it is what
        // makes "reading" mean something at chapter one.
        WatchStatus::Reading => params.base_reading * depth.mul_add(0.6, 0.4),
        WatchStatus::Paused => params.base_paused * depth.mul_add(0.6, 0.4),
        WatchStatus::Planned => params.base_planned,
        WatchStatus::Dropped => depth.mul_add(params.dropped_span, params.dropped_floor),
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

    /// The shipped curve, read out of the registry the console edits.
    fn shipped() -> AffinityParams {
        AffinityParams::default()
    }

    fn affinity(interaction: Interaction) -> f32 {
        super::affinity(interaction, &shipped())
    }

    fn engagement(chapters_read: i64) -> f32 {
        super::engagement(chapters_read, shipped().engagement_knee)
    }

    fn recency(age_days: f32) -> f32 {
        let p = shipped();
        super::recency(age_days, p.recency_half_life_days, p.recency_floor)
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

    /// **Depth never exceeds 1, at any knee.**
    ///
    /// The bug: `engagement` was a bare `ln(read + 1) / ln(knee + 1)`, which passes 1 the moment a
    /// reader goes past the knee — 199 chapters against the shipped knee of 60 gives 1.2888544.
    /// `affinity` clamped its own copy, so the score stayed sane and nothing showed until the
    /// value was *stored*: `user_series_affinity.engagement` is `CHECK (… <= 1)`, so every taste
    /// profile rebuild for a reader with one long series 500'd, and the reader could never load
    /// their recommendations at all.
    ///
    /// The whole knee range is swept because the ceiling is a property of the function, not of the
    /// shipped default.
    #[test]
    fn engagement_stays_within_the_range_the_affinity_table_accepts() {
        for knee in [5.0, 60.0, 1_000.0] {
            for read in [1, 2, 59, 60, 61, 199, 5_000, i64::MAX] {
                let depth = super::engagement(read, knee);
                assert!(
                    (0.0..=1.0).contains(&depth),
                    "engagement({read}, {knee}) = {depth} is outside [0, 1]"
                );
            }
        }
    }

    /// An old favourite is weaker evidence, never no evidence.
    #[test]
    fn recency_decays_to_a_floor_and_not_to_zero() {
        assert!((recency(0.0) - 1.0).abs() < 1e-6);
        assert!(recency(180.0) < recency(0.0));
        assert!(recency(180.0) > recency(720.0));
        assert!(
            recency(10_000.0) >= shipped().recency_floor,
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

    /// **Every knob on this curve has to move the number.**
    ///
    /// The bug this pins is §8.4's: a value published in the tuning console that reaches nothing.
    /// An operator changes it, sees no difference, changes it again, and concludes the page is
    /// broken — which is indistinguishable from the page actually being broken. One assertion per
    /// field, because a parameter struct that is threaded through but only half consumed looks
    /// wired from every angle except the output.
    #[test]
    fn changing_any_affinity_parameter_changes_the_number_it_names() {
        let reading = Interaction {
            status: WatchStatus::Reading,
            chapters_read: 10,
            age_days: 200.0,
        };
        let moved = |mutate: fn(&mut AffinityParams), interaction: Interaction| {
            let mut params = shipped();
            mutate(&mut params);
            (super::affinity(interaction, &params) - super::affinity(interaction, &shipped())).abs()
        };

        assert!(moved(|p| p.base_reading = 0.2, reading) > 1e-4, "reading");
        assert!(moved(|p| p.engagement_knee = 500.0, reading) > 1e-4, "knee");
        assert!(
            moved(|p| p.recency_half_life_days = 20.0, reading) > 1e-4,
            "half-life"
        );
        assert!(
            moved(|p| p.recency_floor = 0.9, reading) > 1e-4,
            "recency floor"
        );
        assert!(
            moved(|p| p.base_completed = 0.5, at(WatchStatus::Completed, 10)) > 1e-4,
            "completed"
        );
        assert!(
            moved(|p| p.base_paused = 0.1, at(WatchStatus::Paused, 10)) > 1e-4,
            "paused"
        );
        assert!(
            moved(|p| p.base_planned = 0.9, at(WatchStatus::Planned, 0)) > 1e-4,
            "planned"
        );
        assert!(
            moved(|p| p.dropped_floor = -0.1, at(WatchStatus::Dropped, 3)) > 1e-4,
            "dropped floor"
        );
        assert!(
            moved(|p| p.dropped_span = 0.05, at(WatchStatus::Dropped, 150)) > 1e-4,
            "dropped span"
        );
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
