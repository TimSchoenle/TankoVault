//! # tankovault-solver
//!
//! The modular bot-management bypass tier (design §9). Three pieces, all solver-agnostic
//! so a new back-end is a one-method drop-in:
//!
//! - [`detect_challenge`] — a cheap, allocation-light classifier run on every response.
//!   On a normal page it is a couple of comparisons; only a positive hit triggers a solve.
//! - [`ChallengeSolver`] — the trait every back-end implements.
//! - [`FlareSolverrSolver`] — the default back-end (talks to a `FlareSolverr` companion).
//!
//! The crate deliberately does **not** depend on `tankovault-fetch`; detection operates over
//! the minimal [`ResponseView`] trait that the fetch layer implements, avoiding a cycle.

mod detection;
mod fake;
mod flaresolverr;
mod types;

pub use detection::{ResponseView, detect_challenge};
pub use fake::StaticSolver;
pub use flaresolverr::FlareSolverrSolver;
pub use types::{ChallengeKind, ChallengeSolver, SolveError, SolveOutcome, SolveRequest};
