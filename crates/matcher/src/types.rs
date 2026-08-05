//! The tuning surface: the thresholds a decision is taken against, and the scored
//! assessment a decision is taken from.

use tankovault_domain::matching::MatchSignals;

/// Confidence thresholds for the decision bands.
#[derive(Debug, Clone, Copy)]
pub struct Thresholds {
    pub high: f32,
    pub low: f32,
    /// At or above this score — **and** only with a structural identity signal — two series
    /// that already exist separately are merged without asking. See [`adjudicate`].
    pub auto_merge: f32,
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
        }
    }
}

/// A scored pair, with the rules that produced the score.
#[derive(Debug, Clone, Copy)]
pub struct Assessment {
    pub score: f32,
    pub signals: MatchSignals,
}
