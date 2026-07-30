//! Series canonicalisation: the vocabulary two layers exchange when deciding whether a
//! scanned or imported series is one the catalogue already holds, and the port through which
//! the persistence layer asks.
//!
//! # Why the port lives here and not in `crates/db` (ARCH-16 step 3)
//!
//! Canonicalisation is a *policy* — how similar is similar enough, and what happens in the
//! band where nothing is certain — but it has to run **inside** the ingest transaction,
//! because each entry of a catalogue page resolves against the series its predecessors created
//! in that same transaction (PERF-15). Those two facts pull in opposite directions: the
//! transaction belongs to the repository layer, the decision does not.
//!
//! So the repository reads the candidates and performs the outcome, and asks a
//! [`Canonicaliser`] supplied by its caller what the outcome *is*. `crates/db` therefore links
//! no scorer and knows no threshold; `tankovault_matcher` scores, and
//! `tankovault_config::MatchingConfig` is the configured policy that implements this trait.
//!
//! The types are here rather than in `tankovault_matcher` for one reason: they are the seam,
//! and a seam whose types live above the crate that has to name them is not a seam. Keeping
//! [`Candidate`] as the *only* candidate type also deletes the hand-written row-to-scorer
//! conversion that ARCH-16 step 1 had merely deduplicated — there is nothing left to convert.

use crate::{ContentType, SeriesId};

/// A candidate existing series to match against (from `db::repo::matching::find_candidates`).
#[derive(Debug, Clone)]
pub struct Candidate {
    pub series_id: SeriesId,
    pub normalized_title: String,
    /// Best trigram similarity in `[0,1]` across the candidate's canonical + alternative titles.
    pub similarity: f32,
    pub content_type: ContentType,
    pub release_year: Option<i32>,
    /// Genre/tag names attached to this series. Empty when unavailable to the caller — the
    /// tag-overlap bonus in `tankovault_matcher::score` simply never fires.
    pub tags: Vec<String>,
    /// Author/artist credits attached to this series (same empty-means-no-signal contract).
    pub authors: Vec<String>,
}

/// The incoming source's identifying attributes.
#[derive(Debug, Clone)]
pub struct Query {
    pub normalized_title: String,
    pub content_type: ContentType,
    pub release_year: Option<i32>,
    pub tags: Vec<String>,
    pub authors: Vec<String>,
}

/// The matching outcome.
#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    /// High confidence: attach the new source to this existing series.
    Attach(SeriesId),
    /// Ambiguous: create the series but flag a merge candidate for operator review.
    Ambiguous { candidate: SeriesId, score: f32 },
    /// Low/no confidence: create a new canonical series.
    Create,
}

/// The canonicalisation policy the ingest paths defer to.
///
/// Both methods are **pure**: the same query and candidates give the same answer, with no I/O.
/// That is what lets the repository call [`Self::canonicalise`] once per entry from inside a
/// transaction it owns, preserving the per-entry resolution PERF-15 depends on, without the
/// policy needing a connection or the repository needing a scorer.
pub trait Canonicaliser: Send + Sync {
    /// How many trigram candidates the repository should fetch and hand over.
    ///
    /// A policy knob rather than a persistence one: it decides how wide to look before
    /// concluding "nothing matches". More costs a wider index scan and buys nothing once the
    /// true match is in the set.
    fn candidate_limit(&self) -> i64;

    /// Decide what to do with `query`, given the candidates the repository found for it.
    fn canonicalise(&self, query: &Query, candidates: &[Candidate]) -> Decision;
}
