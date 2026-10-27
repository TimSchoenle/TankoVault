//! The tuning surface: the thresholds a decision is taken against, the scored assessment a
//! decision is taken from, and the itemised account of how that score was reached.

use tankovault_domain::matching::{MatchSignals, MergeVerdict};

/// Confidence thresholds for the decision bands, plus the guards an automatic merge must clear.
#[derive(Debug, Clone, Copy)]
pub struct Thresholds {
    /// At or above this score, and only with a primary identity and no veto,
    /// [`decide`](crate::decide) attaches a scanned source to the existing series with no review.
    pub high: f32,
    /// The review floor. Below it a pair is not queued at all: `decide` creates a new series
    /// and [`adjudicate`](crate::adjudicate) reports [`MergeVerdict::Distinct`].
    pub low: f32,
    /// At or above this score, two existing series are merged without asking, provided they
    /// share a primary identity and every enabled [`MergeGuards`] guard is silent. See
    /// [`adjudicate`](crate::adjudicate).
    pub auto_merge: f32,
    /// Which vetoes are switched on for this deployment.
    pub guards: MergeGuards,
}

/// Signals that veto an automatic merge, each switchable.
///
/// A score cannot express "these titles agree and the works do not". Each guard names one way
/// two series can score identically and still be different works. Each is switchable because
/// the evidence behind it varies by catalogue: a deployment whose providers rarely publish
/// credits gains nothing from [`Self::author_conflict`] and pays for it in review-queue length.
///
/// Switching a guard off does not silence its signal. The signal still fires and is still
/// recorded on the decision; it only stops blocking the merge. The author, year and medium
/// signals never move a score, so these guards are the only way those signals reach a verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "four independent switches, each deserialised from its own configuration key; an \
              enum or a state machine could not be set one key at a time"
)]
pub struct MergeGuards {
    /// Titles carrying different numbers are reported [`MergeVerdict::Distinct`], not held for
    /// review: queueing a sequel asks an operator to re-derive the one fact the scorer is
    /// certain of. This is the only guard whose verdict is not `Review`.
    pub numeric_conflict: bool,
    /// Both sides name authors and share none.
    pub author_conflict: bool,
    /// Release years three or more years apart.
    pub year_conflict: bool,
    /// Both sides declare a medium and they disagree.
    pub type_conflict: bool,
}

impl Default for MergeGuards {
    /// Returns every guard on.
    ///
    /// A guard only ever moves a pair towards review, so the strict default is the safe one: a
    /// wrong guard costs one queue row, and a missing one costs a deleted series.
    fn default() -> Self {
        Self {
            numeric_conflict: true,
            author_conflict: true,
            year_conflict: true,
            type_conflict: true,
        }
    }
}

impl MergeGuards {
    /// Returns the slugs of the enabled guards that fired for `signals`.
    ///
    /// The numeric veto is left out: it yields [`MergeVerdict::Distinct`], not a review, and
    /// [`adjudicate`](crate::adjudicate) applies it first.
    ///
    /// ```
    /// use tankovault_domain::matching::MatchSignals;
    /// use tankovault_matcher::MergeGuards;
    ///
    /// let signals = MatchSignals {
    ///     year_conflict: true,
    ///     numeric_conflict: true,
    ///     ..MatchSignals::default()
    /// };
    /// assert_eq!(MergeGuards::default().blocking(signals), vec!["year_conflict"]);
    ///
    /// let lenient = MergeGuards { year_conflict: false, ..MergeGuards::default() };
    /// assert!(lenient.blocking(signals).is_empty());
    /// ```
    #[must_use]
    pub fn blocking(self, signals: MatchSignals) -> Vec<&'static str> {
        let mut out = Vec::new();
        for (blocks, fired, label) in [
            (
                self.author_conflict,
                signals.author_conflict,
                "author_conflict",
            ),
            (self.year_conflict, signals.year_conflict, "year_conflict"),
            (self.type_conflict, signals.type_conflict, "type_conflict"),
        ] {
            if blocks && fired {
                out.push(label);
            }
        }
        out
    }
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            high: 0.85,
            low: 0.6,
            // Deliberately close to the ceiling. The automatic merge deletes a series row, and
            // the signals that can reach this number without a primary identity are exactly the
            // fuzzy ones an operator should be looking at.
            auto_merge: 0.97,
            guards: MergeGuards::default(),
        }
    }
}

/// A scored pair, with the rules that produced the score.
#[derive(Debug, Clone, Copy)]
pub struct Assessment {
    /// The final similarity, clamped to 0.0..=1.0 after every term is applied.
    pub score: f32,
    /// Which rules fired, recorded whether or not they moved the score.
    pub signals: MatchSignals,
}

/// One term of a score, as the scorer applied it.
///
/// Persisted verbatim onto the decision record, so [`Self::rule`] is a stable slug for the same
/// reason [`MatchSignals::labels`] are: a renamed one silently blanks the explanation on every
/// historical row.
#[derive(Debug, Clone, PartialEq)]
pub struct ScoreTerm {
    /// Stable slug naming the rule.
    pub rule: &'static str,
    /// What the rule added to (or took off) the running score, before the final clamp.
    pub delta: f32,
    /// The values the rule fired on, in one phrase an operator can read without the code.
    pub detail: String,
}

/// A score, its signals, and the full account of how it was reached.
///
/// The itemised half exists because the automatic merge is destructive and human-free: a stored
/// score and a list of signal names tell an operator *that* the pair matched, and this tells them
/// which title on each side matched, by how much each rule moved the number, and therefore
/// whether the answer was reached for the right reason. See [`explain`](crate::explain).
#[derive(Debug, Clone)]
pub struct Explanation {
    /// The verdict-bearing half: the same score and signals a decision is taken from.
    pub assessment: Assessment,
    /// The base similarity before any term: the strongest of the trigram score the database
    /// computed, the token-set ratio and the compact edit ratio.
    pub base: f32,
    /// Every term applied, in the order the scorer applied them.
    pub terms: Vec<ScoreTerm>,
    /// The query-side title that produced the winning comparison.
    pub matched_query_title: String,
    /// The candidate-side title it won against.
    pub matched_candidate_title: String,
    /// Whether the winning candidate title was one of its alternative titles.
    pub via_alias: bool,
}

/// A merge verdict with the rule that produced it.
///
/// The slug matters as much as the verdict. "Not merged" has many causes: below the review
/// floor, below the automatic threshold, no identity, an alias alone, a veto or a guard. An
/// operator triaging thousands of queue rows needs to know which without re-deriving it from a
/// score and a list of signal names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Adjudication {
    /// What to do with the pair.
    pub verdict: MergeVerdict,
    /// Stable slug for the rule that decided it.
    pub reason: &'static str,
    /// Every enabled guard that fired. Non-empty means an otherwise automatic merge was held
    /// back, or would have been had the score reached the threshold.
    pub blocked_by: Vec<&'static str>,
}
