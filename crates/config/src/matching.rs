//! The confidence policy for deciding "is this the same series?".

use serde::Deserialize;

/// The confidence policy for deciding "is this the same series?" (design §10).
///
/// # Why this is shared configuration
///
/// Two code paths make that decision — the worker's ingest canonicalisation
/// (`tankovault_db::repo::catalog::resolve_canonical_series`) and external sync's remote-entry
/// resolution (`services/sync`'s `SeriesResolver`) — using the same scorer. They used to take
/// their thresholds from different places, one of them hardcoded inside the persistence layer, so
/// the worker could attach a source that sync would refuse to map with no single place to reason
/// about it (ARCH-16). Tuning matching is one decision; it is made here.
///
/// The defaults are the scorer's own (`tankovault_matcher::Thresholds::default()`): 0.85 to
/// attach, 0.6 to flag as a merge candidate. Raising `high` makes the matcher more conservative
/// (more duplicate series, fewer wrong merges); lowering it does the reverse.
#[derive(Debug, Clone, Deserialize)]
pub struct MatchingConfig {
    /// At or above this score, attach to the existing series outright.
    #[serde(default = "default_threshold_high")]
    pub high: f32,
    /// At or above this score but below [`Self::high`], create the series but flag the pair for
    /// operator review.
    #[serde(default = "default_threshold_low")]
    pub low: f32,
    /// How many trigram candidates to score per query title. More costs a wider index scan and
    /// buys nothing once the true match is in the set.
    #[serde(default = "default_candidate_limit")]
    pub candidate_limit: i64,
}

fn default_threshold_high() -> f32 {
    tankovault_matcher::Thresholds::default().high
}

fn default_threshold_low() -> f32 {
    tankovault_matcher::Thresholds::default().low
}

fn default_candidate_limit() -> i64 {
    10
}

impl Default for MatchingConfig {
    fn default() -> Self {
        Self {
            high: default_threshold_high(),
            low: default_threshold_low(),
            candidate_limit: default_candidate_limit(),
        }
    }
}

impl MatchingConfig {
    /// The scorer's threshold pair.
    #[must_use]
    pub const fn thresholds(&self) -> tankovault_matcher::Thresholds {
        tankovault_matcher::Thresholds {
            high: self.high,
            low: self.low,
        }
    }
}
