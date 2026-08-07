//! The public decision surface: pick the best candidate, and adjudicate what to do with it.

use tankovault_domain::SeriesId;
use tankovault_domain::matching::{Candidate, Decision, MergeVerdict, Query};

use crate::assess::assess;
use crate::types::{Adjudication, Assessment, Thresholds};

/// The single best-scoring candidate for `query`, with its score — or `None` when there are
/// no candidates. Unlike [`decide`], this reports the raw best so a caller working across
/// several title variants (e.g. romaji/english/native) can pick the global maximum before
/// applying its own threshold.
#[must_use]
pub fn best_match(query: &Query, candidates: &[Candidate]) -> Option<(SeriesId, f32)> {
    best_assessment(query, candidates).map(|(id, a)| (id, a.score))
}

/// As [`best_match`], but keeping the signals that produced the winning score.
#[must_use]
pub fn best_assessment(query: &Query, candidates: &[Candidate]) -> Option<(SeriesId, Assessment)> {
    candidates
        .iter()
        .map(|c| (c.series_id, assess(query, c)))
        .max_by(|a, b| a.1.score.total_cmp(&b.1.score))
}

/// What to do about two series that **already exist** as separate rows, and why.
///
/// [`decide`] answers a different question — where an incoming source belongs, before anything
/// is written. This one is asked by the merge queue and by the automatic-merge sweep, and its
/// affirmative is destructive: [`MergeVerdict::Auto`] ends with one of the two ids no longer
/// existing.
///
/// Which is why the bar is three-part rather than a number.
///
/// 1. A score alone can reach the automatic threshold on nothing but fuzzy similarity, and two
///    genuinely different works with similar names are precisely the pairs that do.
///    [`MatchSignals::is_structural`](crate::MatchSignals::is_structural) means the titles are *the same string* under a rule designed
///    to be conservative — identical, identical modulo whitespace, or an exact hit on a name the
///    series already answers to.
/// 2. The score must clear [`Thresholds::auto_merge`].
/// 3. No enabled [`MergeGuards`](crate::MergeGuards) may have fired. Each guard names a way two series can satisfy
///    both of the above and still be different works: a numbered sequel, a remake three years
///    apart, an adaptation in a different medium, a same-titled work by different creators.
///
/// The returned [`Adjudication`] carries the slug of the rule that decided it, because "not
/// merged" has four meanings and a queue row showing only a score cannot be triaged.
///
/// ```
/// use tankovault_domain::matching::{MatchSignals, MergeVerdict};
/// use tankovault_matcher::{Assessment, MergeGuards, Thresholds, adjudicate};
///
/// let t = Thresholds::default();
/// let structural = MatchSignals { compact_identity: true, ..MatchSignals::default() };
///
/// // Same string modulo whitespace, scoring at the ceiling: merge it.
/// let merged = adjudicate(Assessment { score: 1.0, signals: structural }, t);
/// assert_eq!(merged.verdict, MergeVerdict::Auto);
/// assert_eq!(merged.reason, "structural_identity_above_threshold");
///
/// // The same score with no identity rule behind it is exactly what review is for.
/// let fuzzy = MatchSignals { near_identical: true, ..MatchSignals::default() };
/// let held = adjudicate(Assessment { score: 1.0, signals: fuzzy }, t);
/// assert_eq!(held.verdict, MergeVerdict::Review);
/// assert_eq!(held.reason, "no_structural_identity");
///
/// // A numeric conflict is never merged automatically, whatever else agrees: a sequel's
/// // title resembles its predecessor's more closely the better the matcher works.
/// let sequel = MatchSignals { numeric_conflict: true, ..structural };
/// let apart = adjudicate(Assessment { score: 1.0, signals: sequel }, t);
/// assert_eq!(apart.verdict, MergeVerdict::Distinct);
/// assert_eq!(apart.reason, "numeric_conflict");
///
/// // A guard holds an otherwise-automatic merge for review, and names itself.
/// let remake = MatchSignals { year_conflict: true, ..structural };
/// let blocked = adjudicate(Assessment { score: 1.0, signals: remake }, t);
/// assert_eq!(blocked.verdict, MergeVerdict::Review);
/// assert_eq!(blocked.blocked_by, vec!["year_conflict"]);
///
/// // Switching that guard off restores the merge without changing the score or the signal.
/// let permissive = Thresholds {
///     guards: MergeGuards { year_conflict: false, ..MergeGuards::default() },
///     ..Thresholds::default()
/// };
/// assert_eq!(
///     adjudicate(Assessment { score: 1.0, signals: remake }, permissive).verdict,
///     MergeVerdict::Auto,
/// );
/// ```
#[must_use]
pub fn adjudicate(assessment: Assessment, thresholds: Thresholds) -> Adjudication {
    let Assessment { score, signals } = assessment;
    if thresholds.guards.numeric_conflict && signals.numeric_conflict {
        // Not merely "do not merge automatically": a pair whose titles carry different numbers
        // is reported as distinct rather than queued, because queueing it asks an operator to
        // re-derive the one fact the scorer is already certain about.
        return Adjudication {
            verdict: MergeVerdict::Distinct,
            reason: "numeric_conflict",
            blocked_by: vec!["numeric_conflict"],
        };
    }

    let blocked_by = thresholds.guards.blocking(signals);
    let verdict = |verdict, reason| Adjudication {
        verdict,
        reason,
        blocked_by: blocked_by.clone(),
    };

    if score < thresholds.low {
        return verdict(MergeVerdict::Distinct, "below_review_floor");
    }
    if !signals.is_structural() {
        return verdict(MergeVerdict::Review, "no_structural_identity");
    }
    if score < thresholds.auto_merge {
        return verdict(MergeVerdict::Review, "below_auto_merge_threshold");
    }
    if let Some(guard) = blocked_by.first().copied() {
        return verdict(MergeVerdict::Review, guard);
    }
    verdict(MergeVerdict::Auto, "structural_identity_above_threshold")
}

/// Decide how to canonicalise the query given its candidates and thresholds.
///
/// Three outcomes, and the middle one is why this is a function rather than a comparison:
/// at or above [`Thresholds::high`] the source attaches to the existing series, below
/// [`Thresholds::low`] a new canonical series is created, and *between* them nothing is
/// decided automatically — the source is created and a merge candidate is queued for an
/// operator. Collapsing that band into either neighbour is how a catalogue either splits one
/// work across two entries or silently merges two different ones.
///
/// Only the best-scoring candidate is considered, so a long candidate list cannot outvote it.
///
/// The realistic case — the same work listed by a second provider:
///
/// ```
/// use tankovault_domain::{ContentType, SeriesId};
/// use tankovault_matcher::{Candidate, Decision, Query, Thresholds, decide};
///
/// let existing = SeriesId::new();
/// let query = Query {
///     normalized_title: "solo leveling".to_owned(),
///     content_type: ContentType::Manhwa,
///     release_year: Some(2018),
///     tags: Vec::new(),
///     authors: Vec::new(),
/// };
/// let same_work = Candidate {
///     series_id: existing,
///     normalized_title: "solo leveling".to_owned(),
///     // Even a poor raw trigram score attaches here: [`score`] takes the *strongest* of the
///     // trigram similarity, a token-set ratio and a compact comparison, and identical titles
///     // agree completely under all three.
///     similarity: 0.2,
///     alt_normalized_titles: Vec::new(),
///     content_type: ContentType::Manhwa,
///     release_year: Some(2018),
///     tags: Vec::new(),
///     authors: Vec::new(),
/// };
/// assert_eq!(
///     decide(&query, &[same_work], Thresholds::default()),
///     Decision::Attach(existing),
/// );
/// ```
///
/// The three bands, with every corroborating signal switched off (unknown medium, no year,
/// unrelated titles) so the score *is* the trigram similarity and the boundaries are visible:
///
/// ```
/// use tankovault_domain::{ContentType, SeriesId};
/// use tankovault_matcher::{Candidate, Decision, Query, Thresholds, decide};
///
/// let existing = SeriesId::new();
/// let query = Query {
///     normalized_title: "solo leveling".to_owned(),
///     content_type: ContentType::Unknown,
///     release_year: None,
///     tags: Vec::new(),
///     authors: Vec::new(),
/// };
/// let scoring = |similarity| Candidate {
///     series_id: existing,
///     normalized_title: "berserk".to_owned(),
///     similarity,
///     alt_normalized_titles: Vec::new(),
///     content_type: ContentType::Unknown,
///     release_year: None,
///     tags: Vec::new(),
///     authors: Vec::new(),
/// };
///
/// assert_eq!(
///     decide(&query, &[scoring(0.9)], Thresholds::default()),
///     Decision::Attach(existing),
/// );
/// assert!(matches!(
///     decide(&query, &[scoring(0.7)], Thresholds::default()),
///     Decision::Ambiguous { candidate, .. } if candidate == existing
/// ));
/// assert_eq!(
///     decide(&query, &[scoring(0.3)], Thresholds::default()),
///     Decision::Create,
/// );
///
/// // No candidates at all is the same answer as a bad one: the first source of a work has
/// // nothing to match against.
/// assert_eq!(decide(&query, &[], Thresholds::default()), Decision::Create);
/// ```
#[must_use]
pub fn decide(query: &Query, candidates: &[Candidate], thresholds: Thresholds) -> Decision {
    match best_assessment(query, candidates) {
        // The numeric guard is restated here rather than left to the arithmetic above. The
        // penalty in `assess` is sized to drop a realistic sequel pair out of the bands, but
        // "sized to" is not "guaranteed to", and attaching a sequel's sources onto its
        // predecessor is silent and expensive to undo.
        Some((id, a)) if a.score >= thresholds.high && !a.signals.numeric_conflict => {
            Decision::Attach(id)
        }
        Some((id, a)) if a.score >= thresholds.low => Decision::Ambiguous {
            candidate: id,
            score: a.score,
            signals: a.signals,
        },
        _ => Decision::Create,
    }
}
