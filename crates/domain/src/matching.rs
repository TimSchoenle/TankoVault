//! Series canonicalisation: the vocabulary two layers exchange when deciding whether a scanned
//! or imported series is one the catalogue already holds. `crates/db` owns the transaction and
//! calls a [`Canonicaliser`] it is handed; it links no scorer and knows no threshold.

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
    /// Retrieval can match on a synonym alone; without these the scorer re-scores against a
    /// title the candidate wasn't actually found by, understating an exact alias hit.
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
/// A bare score cannot tell the merge queue *why* two titles matched, and the destructive
/// automatic merge must not fire on a high fuzzy score alone — only on [`Self::is_structural`],
/// which means the titles are the same string under a conservative rule.
///
/// A flat struct of `bool`s rather than a bitflag set: it is `Copy` and each field is named
/// where it is read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "the flat bools are the point, as the doc comment above argues: this is a Copy \
              record of which rules fired, each field read by name. The lint's suggested \
              remedy — a bitflag or an enum — is what it was deliberately not written as."
)]
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
    /// Both sides name authors and they have **none** in common.
    ///
    /// The counterpart to [`Self::shared_author`], and not merely its absence: two series where
    /// one side has no credits at all says nothing, while two series that each name a full
    /// creator list and agree on no one is positive evidence of two different works. A remake, a
    /// spin-off by a different studio and a same-titled unrelated series all present this way.
    pub author_conflict: bool,
    /// The two series share at least one genre/tag.
    pub tag_overlap: bool,
    /// The titles carry **different numbers** — `Overlord` against `Overlord 2`, volume 3
    /// against volume 4. Nothing else in the scorer distinguishes a sequel from its predecessor,
    /// and the closer the rest of the title matches the more certain a merge would be wrong.
    pub numeric_conflict: bool,
}

impl MatchSignals {
    /// Whether the two titles are the *same string* under an identity rule, not merely a high
    /// score — the precondition for an automatic, destructive merge.
    ///
    /// A fuzzy score can reach 0.95 on two genuinely different works with similar names; an
    /// identity rule at the same score means the titles differ only in whitespace or punctuation.
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
            (self.author_conflict, "author_conflict"),
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
    /// Carries signals as well as score — a queue row with only a number cannot be triaged,
    /// re-scored, or safely auto-resolved later.
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
/// Both methods are **pure** — same query and candidates give the same answer, no I/O — so the
/// repository can call [`Self::canonicalise`] once per entry inside a transaction it owns,
/// without the policy needing a connection or the repository needing a scorer.
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
