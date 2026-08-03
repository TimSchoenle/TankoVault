//! # tankovault-solver
//!
//! The modular bot-management bypass tier (design §9): cheap challenge detection, the
//! pluggable [`ChallengeSolver`] contract, and the default [`TrawlSolver`] back-end.
//!
//! Does not depend on `tankovault-fetch`; detection uses the minimal [`ResponseView`] trait
//! instead, to avoid a cycle.

mod detection;
mod fake;
#[cfg(feature = "axum")]
pub mod http;
mod trawl;
mod types;

pub use detection::{ResponseView, detect_challenge, detect_challenge_body, is_rate_limit_page};
pub use fake::StaticSolver;
pub use trawl::TrawlSolver;
pub use types::{ChallengeKind, ChallengeSolver, SolveError, SolveOutcome, SolveRequest};
