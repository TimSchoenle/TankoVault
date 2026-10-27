//! # tankovault-matcher
//!
//! Scores a scanned title against the existing series and decides whether to attach, review or
//! create (`docs/ARCHITECTURE.md` §10, steps 3 and 4).
//!
//! The crate is pure and has no database. [`decide`] scores [`Candidate`]s against a [`Query`]
//! and returns a [`Decision`]. `crates/db` asks for that decision through the [`Canonicaliser`]
//! port in [`tankovault_domain::matching`], so it never links a scorer. [`adjudicate`] decides
//! whether two existing series are one work, and [`adjudicate_pair`] asks it the way the
//! duplicate sweep does.

// The port's types live in the domain crate, where `crates/db` names them. Re-exporting them
// lets a caller of this crate import the whole surface from one place.
pub use tankovault_domain::matching::{
    Candidate, Canonicaliser, Decision, LiveTiming, MatchSignals, MergeVerdict, Query,
};

mod assess;
mod decide;
mod edition;
mod external;
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
pub use external::titles_identify;
pub use pair::{
    LIVE_AGREEMENT_HOURS, LIVE_CONFLICT_HOURS, PairReading, PairSide, adjudicate_pair,
    apply_live_timing, read_pair, wants_live_timing,
};
pub use similarity::token_set_ratio;
pub use types::{Adjudication, Assessment, Explanation, MergeGuards, ScoreTerm, Thresholds};
