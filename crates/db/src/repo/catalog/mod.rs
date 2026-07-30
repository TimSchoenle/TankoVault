//! Catalog write + read path: canonical series, provider sources, and chapters.
//!
//! Every write is an idempotent `INSERT ... ON CONFLICT` so re-running a task under
//! at-least-once delivery is safe (design Appendix A §4). Chapter upserts report which
//! rows were genuinely new — via the `xmax = 0` idiom — so the worker can emit
//! `chapter.discovered` only for real discoveries.
//!
//! # Layout (ARCH-3)
//!
//! One module per aggregate, split at the banner comments the old 1,679-line file already
//! carried:
//!
//! | module | owns |
//! |---|---|
//! | [`series`] | the canonical series row and which existing series a scanned one *is* |
//! | [`enrichment`] | the enrichment work list and the title/tag/author link tables |
//! | [`sources`] | per-provider sources, their scan bookkeeping, and stub registration |
//! | [`chapters`] | chapter upserts, counts and listings |
//! | [`browse`] | the read models behind the discover/browse surfaces |
//! | [`ingest`] | the composite worker transaction that drives all of the above |
//!
//! The modules are public and the glob re-exports below keep every existing
//! `repo::catalog::…` path resolving, so callers can move to the narrower
//! `repo::catalog::chapters::…` spelling one at a time rather than in one sweep.

pub mod browse;
pub mod chapters;
pub mod enrichment;
pub mod ingest;
pub mod series;
pub mod sources;

pub use browse::*;
pub use chapters::*;
pub use enrichment::*;
pub use ingest::*;
pub use series::*;
pub use sources::*;
