//! How much a reader cares about a series, as one number in `[-1, 1]`.
//!
//! Affinity comes from signals the product already collects: watchlist status, reading depth,
//! and when either last moved. There is no rating column, so a reader is never asked to rate.

use tankovault_domain::{Tunable, WatchStatus};

/// The shape of the affinity curve, as an operator has it set.
///
/// [`Default`] reads the compiled defaults from [`Tunable`], the registry the console edits.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AffinityParams {
    /// Affinity for a finished series, the reference the other bases are read against.
    pub base_completed: f32,
    /// Base for a series currently being read, before depth scaling.
    pub base_reading: f32,
    /// Base for a paused series, before depth scaling.
    pub base_paused: f32,
    /// Base for a plan-to-read entry. It records intent, not taste, so depth does not scale it.
    pub base_planned: f32,
    /// Affinity for a series abandoned immediately. Negative.
    pub dropped_floor: f32,
    /// How far a fully committed reader claws back from [`Self::dropped_floor`].
    pub dropped_span: f32,
    /// Chapters at which a reader counts as fully committed.
    ///
    /// Depth is measured in absolute chapters, not as a fraction of the series. A fraction would
    /// score a reader at chapter 300 of a 900-chapter ongoing series as two thirds from an end
    /// that does not exist yet.
    pub engagement_knee: f32,
    /// Days after which a signal counts half as much.
    pub recency_half_life_days: f32,
    /// The floor the recency decay never falls through.
    ///
    /// Without a floor, a dormant reader's profile decays to noise. A favourite read five years ago
    /// is weaker evidence about taste than last week, but still evidence.
    pub recency_floor: f32,
}

impl Default for AffinityParams {
    fn default() -> Self {
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

/// Returns reading depth on `[0, 1]`, log-scaled and reaching 1 at `knee` chapters.
///
/// Chapter 3 against chapter 30 says far more about investment than chapter 300 against 330,
/// hence the logarithm. The result never exceeds 1, which `user_series_affinity.engagement`
/// requires. A non-positive count is depth 0, and a knee below 1 is treated as 1.
///
/// ```
/// use tankovault_recsys::affinity::engagement;
///
/// assert!(engagement(0, 60.0).abs() < f32::EPSILON);
/// assert!(engagement(3, 60.0) < engagement(30, 60.0));
/// assert!((engagement(5_000, 60.0) - 1.0).abs() < f32::EPSILON);
/// ```
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
    // The registry floors the knee at five, but params can be built by hand, and a knee at or
    // below zero would flip the sign.
    ((read + 1.0).ln() / (knee.max(1.0) + 1.0).ln()).min(1.0)
}

/// Returns `0.5^(age / half_life)`, never below `floor`, so an old favourite still counts.
#[must_use]
pub fn recency(age_days: f32, half_life_days: f32, floor: f32) -> f32 {
    let decayed = 0.5_f32.powf(age_days.max(0.0) / half_life_days.max(f32::EPSILON));
    decayed.max(floor)
}

/// Returns the reader's affinity for one series.
///
/// # `dropped` is not one signal
///
/// Dropped at chapter 3 means "wrong for me". Dropped at chapter 150 means "I liked this for a
/// long time and then it declined", which is positive about everything except the ending. Scoring
/// both `-1` would point a long-series reader's strongest negatives at what they read most of.
///
/// So `dropped` interpolates: strongly negative when abandoned early, barely negative when
/// abandoned deep.
///
/// # `planned` is intent, not taste
///
/// A plan-to-read list is full of things people never open, so it sits below every status that
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
        // Two chapters in says much less than two hundred, and both are "reading". The 0.4 is the
        // share a held series keeps at any depth, so "reading" means something at chapter one;
        // it is not a tunable.
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

    /// Dropping a series after 150 chapters is a far weaker negative than dropping it after 3.
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

    /// A plan to read counts for less than any status with chapters read.
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

    /// Reading deeper raises affinity.
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

    /// Depth never exceeds 1 at any knee, since `user_series_affinity.engagement` is
    /// `CHECK (… <= 1)` and a stored value above it fails the taste-profile rebuild.
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

    /// Recency decays to its floor and never to zero.
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

    /// Recency scales a signal and never changes its sign.
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

    /// Every field of [`AffinityParams`] moves the number it names, so no tunable in the console
    /// is inert. One assertion per field.
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
