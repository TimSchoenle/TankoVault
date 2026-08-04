//! The recommendation model, as pure functions over plain data.
//!
//! No `sqlx`, no `axum`, no I/O: everything here is testable — and property-testable — without a
//! database, and the same code serves a full catalogue rebuild and a single-series repair.
//! `crates/db/src/repo/recsys` reads and writes; `services/worker` drives; this crate decides
//! what a series *is* and how close two of them are.
//!
//! The pipeline, in the order the builder runs it:
//!
//! 1. [`features::extract`] turns a series' facts into a weighted feature bag, and
//!    [`features::digest`] fingerprints it so an unchanged series can be skipped.
//! 2. [`weighting::idf`] and [`weighting::apply_idf`] make those weights discriminating and the
//!    vectors comparable.
//! 3. [`embedding::GramAccumulator`] streams the catalogue into a `d x d` covariance matrix and
//!    [`embedding::GramAccumulator::basis`] reduces it to a projection; [`embedding::Basis`]
//!    applies it.
//! 4. [`similarity`] scores and explains a candidate pair exactly, against the sparse vectors the
//!    embedding was derived from.
//!
//! The reader's half is two more modules, used by the request path rather than the builder:
//! [`affinity`] turns watchlist status and reading depth into one number per series, and
//! [`ranking`] blends the retrieval paths and diversifies the result.
//!
//! Retrieval itself is not here: it is an HNSW search, which belongs to Postgres.

pub mod affinity;
pub mod embedding;
pub mod features;
pub mod ranking;
pub mod similarity;
pub mod weighting;

pub use affinity::{Interaction, affinity};
pub use embedding::{Basis, GramAccumulator};
pub use features::{FeatureKey, FeatureKind, SeriesFacts, digest, extract, length_bucket};
pub use ranking::{Candidate, Path, Scored, blend, cap_by, diversify};
pub use similarity::{cosine, shared_features};
pub use weighting::{apply_idf, idf, normalise};

/// The width of the dense space, and the type of `series_embedding.embedding`.
///
/// A constant rather than a parameter because the column is declared `halfvec(128)`: changing it
/// is a column-type change and a full re-embed, not a tuning knob, and a mismatch between this
/// and the schema is a write that fails at run time on every row.
pub const EMBEDDING_DIMS: usize = 128;

/// How many features may shape the dense space.
///
/// The projection's input dimension, and therefore the side of the covariance matrix: at 2 048
/// that is ~32 MB, which is the whole of the builder's non-streaming memory. Features past the
/// cap (by document frequency) still score and explain — they simply do not steer the geometry.
pub const DENSE_INPUT_CAP: usize = 2_048;

/// Orthogonal-iteration passes when solving for the basis.
///
/// Convergence is geometric in the ratio between consecutive eigenvalues. This is well past the
/// point where the *ranking* the basis produces stops moving, which is a far weaker requirement
/// than converging the eigenvectors themselves.
pub const BASIS_ITERATIONS: usize = 32;
