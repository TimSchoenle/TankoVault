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
    /// The candidate's **alternative** normalized titles (`series_titles.normalized`).
    ///
    /// The trigram lookup that produced this candidate already searches alternative titles, so
    /// a series can be returned entirely on the strength of a synonym — but the scorer used to
    /// see only `normalized_title` and would then re-score that same candidate against a title
    /// it does not have. Carrying the alternatives makes the scoring symmetric with the
    /// retrieval: an exact or whitespace-insensitive hit on a synonym counts for as much as one
    /// on the canonical title, which is what it is worth.
    pub alt_normalized_titles: Vec<String>,
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

/// Which scoring rules fired for one query/candidate pair.
///
/// # Why the scorer reports this and not only a number
///
/// A score of 0.86 says nothing about *why*, and two things downstream need to know. The merge
/// queue's `reason` column was the constant string `"ambiguous title match"` for every row, so
/// an operator triaging 2 600 candidates had the two titles and a percentage and nothing else.
/// And the automatic merge — which deletes a series — must not fire on a score alone: a high
/// number produced entirely by fuzzy similarity is exactly the case a human should look at,
/// whereas [`Self::is_structural`] means the two titles are *the same string* under a rule
/// whose whole job is to be conservative.
///
/// Deliberately a flat struct of `bool`s rather than a bitflag set: it is `Copy`, it appears in
/// [`Decision`], and each field is named where it is read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MatchSignals {
    /// The two normalized titles are byte-identical.
    pub exact_title: bool,
    /// The two normalized titles are identical once whitespace is removed — the "provider
    /// dropped a space between two HTML elements" class (`Spy X Family` / `Spyxfamily`).
    pub compact_identity: bool,
    /// The query matched one of the candidate's *alternative* titles exactly, or exactly
    /// modulo whitespace.
    pub alias_identity: bool,
    /// The titles are not identical but are within a couple of characters of each other, long
    /// enough for that to mean something.
    pub near_identical: bool,
    /// Every word of one title appears in the other (an abbreviated vs. a subtitled edition).
    pub containment: bool,
    /// Both sides declare a medium and it is the same one.
    pub type_agreement: bool,
    /// Both sides declare a medium and they disagree.
    pub type_conflict: bool,
    /// Both sides carry a release year within a year of each other.
    pub year_agreement: bool,
    /// Both sides carry a release year three or more years apart.
    pub year_conflict: bool,
    /// The two series share at least one author/artist credit.
    pub shared_author: bool,
    /// The two series share at least one genre/tag.
    pub tag_overlap: bool,
    /// The titles carry **different numbers** — `Overlord` against `Overlord 2`, volume 3
    /// against volume 4. Nothing else in the scorer distinguishes a sequel from its predecessor,
    /// and the closer the rest of the title matches the more certain a merge would be wrong.
    pub numeric_conflict: bool,
}

impl MatchSignals {
    /// Whether the two titles are the *same string* under one of the identity rules, as opposed
    /// to merely scoring highly.
    ///
    /// This is the precondition for an automatic, destructive merge. A fuzzy score can reach
    /// 0.95 on two genuinely different works with similar names; an identity rule reaching the
    /// same score means the titles differ only in whitespace, punctuation or which of the
    /// series' recorded names was compared.
    #[must_use]
    pub const fn is_structural(self) -> bool {
        self.exact_title || self.compact_identity || self.alias_identity
    }

    /// The stable slugs for the rules that fired, for the merge queue's `reason` column and the
    /// operator console's badges.
    ///
    /// Stable because they are persisted and rendered: a renamed slug silently blanks a badge
    /// on every historical row.
    #[must_use]
    pub fn labels(self) -> Vec<&'static str> {
        let mut out = Vec::new();
        for (fired, label) in [
            (self.exact_title, "exact_title"),
            (self.compact_identity, "compact_identity"),
            (self.alias_identity, "alias_identity"),
            (self.near_identical, "near_identical"),
            (self.containment, "containment"),
            (self.type_agreement, "type_agreement"),
            (self.type_conflict, "type_conflict"),
            (self.year_agreement, "year_agreement"),
            (self.year_conflict, "year_conflict"),
            (self.shared_author, "shared_author"),
            (self.tag_overlap, "tag_overlap"),
            (self.numeric_conflict, "numeric_conflict"),
        ] {
            if fired {
                out.push(label);
            }
        }
        out
    }
}

/// The matching outcome.
#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    /// High confidence: attach the new source to this existing series.
    Attach(SeriesId),
    /// Ambiguous: create the series but flag a merge candidate for operator review.
    ///
    /// Carries the signals as well as the score, because the queue row is written from this and
    /// a row that records only a number cannot be triaged, re-scored or safely auto-resolved
    /// later.
    Ambiguous {
        candidate: SeriesId,
        score: f32,
        signals: MatchSignals,
    },
    /// Low/no confidence: create a new canonical series.
    Create,
}

/// What to do with two series that already exist separately.
///
/// Distinct from [`Decision`], which answers "where does this incoming source belong?" while
/// nothing has been written yet. This one answers "should these two rows become one?", and the
/// affirmative is destructive: the absorbed series' id stops existing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeVerdict {
    /// Merge without asking. Requires both a structural identity signal and a score at or above
    /// the configured automatic-merge threshold.
    Auto,
    /// Put it in front of an operator.
    Review,
    /// Not the same work; do not queue it.
    Distinct,
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
