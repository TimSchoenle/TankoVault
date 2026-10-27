//! The recommendation model, as pure functions over plain data.
//!
//! The crate does no I/O, so every function is tested without a database, and the same code
//! serves a full catalogue rebuild and a single-series repair. `crates/db/src/repo/recsys` reads
//! and writes, `services/worker` drives the build, and this crate decides what a series is and
//! how close two series are.
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
//! The request path uses two more modules: [`mod@affinity`] turns watchlist status and reading depth into one number per series, and
//! [`ranking`] blends the retrieval paths and diversifies the result.
//!
//! Retrieval is an HNSW search in Postgres, outside this crate.

pub mod affinity;
pub mod embedding;
pub mod features;
pub mod ranking;
pub mod similarity;
pub mod weighting;

pub use affinity::{AffinityParams, Interaction, affinity};
pub use embedding::{Basis, GramAccumulator};
pub use features::{FeatureKey, FeatureKind, SeriesFacts, digest, extract, length_bucket};
pub use ranking::{Candidate, Path, PathWeights, Scored, blend, cap_by, cap_by_seed, diversify};
pub use similarity::{cosine, shared_features};
pub use weighting::{apply_idf, idf, normalise};

/// The width of the dense space, and the type of `series_embedding.embedding`.
///
/// The column is declared `halfvec(128)`, so changing this is a column-type change and a full
/// re-embed. A mismatch with the schema fails every write at run time.
pub const EMBEDDING_DIMS: usize = 128;

/// How many features may shape the dense space.
///
/// The projection's input dimension, and so the side of the covariance matrix. At 2,048 the
/// matrix is about 32 MB, the whole of the builder's non-streaming memory. Features past the cap,
/// by document frequency, still score and explain but do not shape the geometry.
pub const DENSE_INPUT_CAP: usize = 2_048;

/// Orthogonal-iteration passes when solving for the basis.
///
/// Convergence is geometric in the ratio between consecutive eigenvalues. 32 passes is well past
/// the point where the ranking the basis produces stops moving, which needs far less than
/// converged eigenvectors.
pub const BASIS_ITERATIONS: usize = 32;
