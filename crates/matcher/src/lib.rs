//! # tankovault-matcher
//!
//! Series canonicalisation scoring (design §10, steps 3–4): pure and DB-free, this crate scores
//! [`Candidate`]s against a [`Query`] and returns a [`Decision`] via the [`Canonicaliser`] port
//! defined in [`tankovault_domain::matching`], so `crates/db` can ask for a decision without
//! linking a scorer (ARCH-16). [`adjudicate`] answers the separate question of merging two
//! series that already exist, and [`adjudicate_pair`] asks it the way the duplicate sweep does.

// Re-exported from tankovault_domain::matching (the ARCH-16 seam `crates/db` names) so
// `tankovault_matcher::Candidate` and friends still resolve.
pub use tankovault_domain::matching::{
    Candidate, Canonicaliser, Decision, MatchSignals, MergeVerdict, Query,
};

mod assess;
mod decide;
mod pair;
mod similarity;
mod title;
mod types;

#[cfg(feature = "replay")]
pub mod replay;

#[cfg(test)]
mod tests;

pub use assess::{assess, explain, score};
pub use decide::{adjudicate, best_assessment, best_match, decide};
pub use pair::{PairReading, PairSide, adjudicate_pair, read_pair};
pub use similarity::token_set_ratio;
pub use types::{Adjudication, Assessment, Explanation, MergeGuards, ScoreTerm, Thresholds};
