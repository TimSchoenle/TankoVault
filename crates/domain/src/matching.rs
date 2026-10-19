//! The vocabulary two layers exchange to decide whether a scanned series is already held.
//!
//! `crates/db` owns the transaction and calls a [`Canonicaliser`] it is handed. It links no
//! scorer and knows no threshold.

use crate::{ContentType, SeriesId};

/// A candidate existing series to match against (from `db::repo::matching::find_candidates`).
#[derive(Debug, Clone)]
pub struct Candidate {
    /// The existing series a match would attach the incoming source to.
    pub series_id: SeriesId,
    /// The candidate's own canonical title, already normalized.
    pub normalized_title: String,
    /// Best trigram similarity in `[0,1]` across the candidate's canonical + alternative titles.
    pub similarity: f32,
    /// The candidate's **alternative** normalized titles (`series_titles.normalized`).
    ///
    /// Retrieval can match on a synonym alone; without these the scorer re-scores against a
    /// title the candidate wasn't actually found by, understating an exact alias hit.
    pub alt_normalized_titles: Vec<String>,
    /// The normalized listing titles of the candidate's sources
    /// (`series_sources.provider_title_normalized`). What a provider calls the work, so a hit on
    /// one is a primary identity, unlike a hit on an alternative title.
    pub source_titles: Vec<String>,
    /// Medium the candidate is recorded under.
    pub content_type: ContentType,
    /// Year the candidate is recorded under, `None` when it has none.
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
    /// The incoming title, normalized the same way a candidate's is.
    pub normalized_title: String,
    /// Medium the source states, `Unknown` when it states none.
    pub content_type: ContentType,
    /// Year the source states, `None` when it states none.
    pub release_year: Option<i32>,
    /// Genre names the source carries. Empty means no signal, not no tags.
    pub tags: Vec<String>,
    /// Credits the source carries, under the same empty-means-no-signal rule.
    pub authors: Vec<String>,
}

/// Which scoring rules fired for one query/candidate pair.
///
/// A bare score cannot tell the merge queue *why* two titles matched, and the destructive
/// automatic merge must not fire on a high fuzzy score alone — only on
/// [`Self::is_primary_identity`] (the titles are the same string under a conservative rule) or
/// [`Self::is_corroborated_alias`].
///
/// Author, year, medium and tags are recorded here and rendered, but they never move a score or
/// a verdict: across providers they are wrong or inconsistent too often to decide anything.
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
    /// modulo whitespace. Evidence, not identity: an alias alone never merges or attaches.
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
    /// The query matched another source's *listing* title exactly, or exactly modulo
    /// whitespace. A listing title is what a provider calls the work, so this is identity.
    pub source_title_identity: bool,
    /// One title carries an edition or arc marker the other lacks (`gaiden`, `colored`, an
    /// `-hen` arc, `Railgun S` against `Railgun T`). A veto.
    pub edition_conflict: bool,
    /// Both series are mapped to the same external tracker id.
    pub external_id_agreement: bool,
    /// Both series are mapped to an external tracker and the ids differ. A veto.
    pub external_id_conflict: bool,
    /// Chapters both series first saw after they were already being scanned arrived within
    /// hours of each other.
    pub live_timing_agreement: bool,
    /// Those chapters arrived weeks apart. A veto.
    pub live_timing_conflict: bool,
}

impl MatchSignals {
    /// Whether a *primary* title — the canonical one, or a provider's listing title — is the
    /// same string on both sides, not merely a high score.
    ///
    /// A fuzzy score can reach 0.95 on two genuinely different works with similar names; an
    /// identity rule at the same score means the titles differ only in whitespace or punctuation.
    /// An alternative title is deliberately not enough: aliases are scraped free text shared
    /// across franchises and junk (`unknown`, `sao`), and alias-only merges pulled a dozen hub
    /// series into absorbing 20–100 different works each.
    #[must_use]
    pub const fn is_primary_identity(self) -> bool {
        self.exact_title || self.compact_identity || self.source_title_identity
    }

    /// Whether an alternative-title hit is corroborated by a trusted signal that is not a title.
    #[must_use]
    pub const fn is_corroborated_alias(self) -> bool {
        self.alias_identity && (self.external_id_agreement || self.live_timing_agreement)
    }

    /// The stable slugs for the rules that fired, for the merge queue's `reason` column and the
    /// operator console's badges.
    ///
    /// Stable because they are persisted and rendered: a renamed slug silently blanks a badge
    /// on every historical row. New slugs are appended, never inserted.
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
            (self.source_title_identity, "source_title_identity"),
            (self.edition_conflict, "edition_conflict"),
            (self.external_id_agreement, "external_id_agreement"),
            (self.external_id_conflict, "external_id_conflict"),
            (self.live_timing_agreement, "live_timing_agreement"),
            (self.live_timing_conflict, "live_timing_conflict"),
        ] {
            if fired {
                out.push(label);
            }
        }
        out
    }
}

/// How the live chapters of two series line up in time: chapters both first saw after both were
/// already being scanned, joined on whole chapter number, for the source pair that agrees best.
///
/// Only first-party discovery times count (`chapters.discovered_at`). Provider publication dates
/// are useless here: mirrors stamp their own upload date, and pairs that are one work showed a
/// mean gap of 1,621 days on them.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct LiveTiming {
    /// Live chapters the best source pair shares.
    pub shared_live_chapters: i64,
    /// The median gap between the two discoveries of those chapters, in hours. `None` when no
    /// source pair shares enough live chapters to say anything.
    pub median_gap_hours: Option<f64>,
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
        /// The existing series the incoming source resembles.
        candidate: SeriesId,
        /// The fuzzy score that landed it in the queue, in `[0,1]`.
        score: f32,
        /// Which rules fired, so the row can be triaged without re-running the scorer.
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
    /// Merge without asking. Requires a primary identity at or above the configured
    /// automatic-merge threshold, or a corroborated alias, and no veto.
    Auto,
    /// Put it in front of an operator.
    Review,
    /// Not the same work; do not queue it.
    Distinct,
}

impl MergeVerdict {
    /// The stable slug `merge_decisions.verdict` stores.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Review => "review",
            Self::Distinct => "distinct",
        }
    }
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
