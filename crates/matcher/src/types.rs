//! The tuning surface: the thresholds a decision is taken against, the scored assessment a
//! decision is taken from, and the itemised account of how that score was reached.

use tankovault_domain::matching::{MatchSignals, MergeVerdict};

/// Confidence thresholds for the decision bands, plus the guards an automatic merge must clear.
#[derive(Debug, Clone, Copy)]
pub struct Thresholds {
    pub high: f32,
    pub low: f32,
    /// At or above this score — **and** only with a structural identity signal, and only with
    /// every enabled [`MergeGuards`] silent — two series that already exist separately are
    /// merged without asking. See [`adjudicate`](crate::adjudicate).
    pub auto_merge: f32,
    pub guards: MergeGuards,
}

/// Signals that veto an automatic merge, each switchable.
///
/// A score is a single number and cannot express "these titles agree and the works do not". Each
/// guard names a specific way two series can score identically and still be different works, and
/// each is separately switchable because the evidence behind it varies by catalogue: a deployment
/// whose providers rarely publish credits gains nothing from [`Self::author_conflict`] and pays
/// for it in review-queue length.
///
/// Switching one **off** does not switch the signal off — it still fires, is still scored, is
/// still recorded on the decision — it only stops that signal blocking the merge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "the flat bools are the point, for the same reason MatchSignals' are: this is a \
              Copy record of which guards are switched on, each field read by name and each \
              deserialised from its own configuration key. The lint's suggested remedy — a \
              state machine or two-variant enums — would turn four independent switches into \
              something a config file cannot express one at a time."
)]
pub struct MergeGuards {
    /// Titles carrying different numbers are reported [`MergeVerdict::Distinct`] rather than
    /// merely un-mergeable: queueing a sequel asks an operator to re-derive the one fact the
    /// scorer is already certain about. This is the only guard whose verdict is not `Review`.
    pub numeric_conflict: bool,
    /// Both sides name authors and share none.
    pub author_conflict: bool,
    /// Release years three or more years apart.
    pub year_conflict: bool,
    /// Both sides declare a medium and they disagree.
    pub type_conflict: bool,
}

impl Default for MergeGuards {
    /// Every guard on. A guard only ever moves a pair *towards* review, so the safe default is
    /// the strict one: the cost of a wrong guard is one queue row, the cost of a missing one is
    /// a deleted series.
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
    /// The guards that fired for `signals`, as their stable slugs, excluding the numeric veto —
    /// which is not a downgrade but an outright [`MergeVerdict::Distinct`] and is handled first.
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
            // the signals that can reach this number without a structural identity match are
            // exactly the fuzzy ones an operator should be looking at.
            auto_merge: 0.97,
            guards: MergeGuards::default(),
        }
    }
}

/// A scored pair, with the rules that produced the score.
#[derive(Debug, Clone, Copy)]
pub struct Assessment {
    pub score: f32,
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
    pub assessment: Assessment,
    /// The base similarity before any term: the strongest of the trigram score the database
    /// computed, the token-set ratio and the compact edit ratio.
    pub base: f32,
    /// Every term applied, in the order the scorer applied them.
    pub terms: Vec<ScoreTerm>,
    /// The query-side title that produced the winning comparison.
    pub matched_query_title: String,
    /// The candidate-side title it won against — the canonical one unless [`Self::via_alias`].
    pub matched_candidate_title: String,
    /// Whether the winning candidate title was an alternative rather than the canonical one.
    pub via_alias: bool,
}

/// A merge verdict with the rule that produced it.
///
/// The slug matters as much as the verdict: "not merged" is the answer for four different
/// reasons — below the review floor, below the automatic threshold, no structural identity, or a
/// guard — and an operator triaging a queue of thousands needs to know which, without re-deriving
/// it from a score and a bag of signal names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Adjudication {
    pub verdict: MergeVerdict,
    /// Stable slug for the rule that decided it.
    pub reason: &'static str,
    /// Every enabled guard that fired. Non-empty means an otherwise-automatic merge was held
    /// back — or would have been, had the score reached the threshold.
    pub blocked_by: Vec<&'static str>,
}
