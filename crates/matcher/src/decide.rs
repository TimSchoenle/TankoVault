//! The public decision surface: pick the best candidate, and adjudicate what to do with it.

use tankovault_domain::SeriesId;
use tankovault_domain::matching::{Candidate, Decision, MatchSignals, MergeVerdict, Query};

use crate::assess::assess;
use crate::types::{Adjudication, Assessment, Thresholds};

/// Returns the best candidate for `query`, as [`best_assessment`] ranks them, with its score.
///
/// No threshold is applied, so a caller scoring several title variants (romaji, english,
/// native) can pick the overall maximum before applying its own.
#[must_use]
pub fn best_match(query: &Query, candidates: &[Candidate]) -> Option<(SeriesId, f32)> {
    best_assessment(query, candidates).map(|(id, a)| (id, a.score))
}

/// Returns the best candidate for `query` with the signals that produced its score.
///
/// A candidate with a primary identity outranks one without, whatever the scores: a reordered
/// title (`another world reincarnation` against `reincarnation another world`) reaches the same
/// clamped 1.0 as an identical one, and only the identical one may attach.
#[must_use]
pub fn best_assessment(query: &Query, candidates: &[Candidate]) -> Option<(SeriesId, Assessment)> {
    candidates
        .iter()
        .map(|c| (c.series_id, assess(query, c)))
        .max_by(|a, b| {
            let identity = |x: &Assessment| x.signals.is_primary_identity();
            identity(&a.1)
                .cmp(&identity(&b.1))
                .then_with(|| a.1.score.total_cmp(&b.1.score))
        })
}

/// Decides what to do about two series that already exist as separate rows, and why.
///
/// [`decide`] places an incoming source before anything is written. This asks a different
/// question for the merge queue and the automatic-merge sweep, and its affirmative is
/// destructive: [`MergeVerdict::Auto`] ends with one of the two ids gone.
///
/// In order:
///
/// 1. A numeric or edition conflict is [`MergeVerdict::Distinct`]: a sequel, a side story or a
///    second arc resembles its predecessor more closely the better the matcher works.
/// 2. Below [`Thresholds::low`] the pair is [`MergeVerdict::Distinct`].
/// 3. An external-id or live-timing conflict holds the pair for review, naming itself.
/// 4. A primary identity ([`MatchSignals::is_primary_identity`](crate::MatchSignals::is_primary_identity))
///    at or above [`Thresholds::auto_merge`] merges, as does a corroborated alias
///    ([`MatchSignals::is_corroborated_alias`](crate::MatchSignals::is_corroborated_alias)),
///    unless an enabled [`MergeGuards`](crate::MergeGuards) guard fired.
/// 5. An alias on its own, a primary identity below the threshold, and a fuzzy score with no
///    identity at all are each reviewed, under their own reason.
///
/// The [`Adjudication`] names the rule that decided it, since a queue row showing only a score
/// cannot be triaged.
///
/// ```
/// use tankovault_domain::matching::{MatchSignals, MergeVerdict};
/// use tankovault_matcher::{Assessment, MergeGuards, Thresholds, adjudicate};
///
/// let t = Thresholds::default();
/// let identical = MatchSignals { compact_identity: true, ..MatchSignals::default() };
///
/// // Same string modulo whitespace, scoring at the ceiling: merge it.
/// let merged = adjudicate(Assessment { score: 1.0, signals: identical }, t);
/// assert_eq!(merged.verdict, MergeVerdict::Auto);
/// assert_eq!(merged.reason, "structural_identity_above_threshold");
///
/// // The same score with no identity rule behind it is exactly what review is for.
/// let fuzzy = MatchSignals { near_identical: true, ..MatchSignals::default() };
/// let held = adjudicate(Assessment { score: 1.0, signals: fuzzy }, t);
/// assert_eq!(held.verdict, MergeVerdict::Review);
/// assert_eq!(held.reason, "no_structural_identity");
///
/// // An alternative title alone is evidence, not identity.
/// let alias = MatchSignals { alias_identity: true, ..MatchSignals::default() };
/// let alone = adjudicate(Assessment { score: 0.8, signals: alias }, t);
/// assert_eq!(alone.verdict, MergeVerdict::Review);
/// assert_eq!(alone.reason, "alias_uncorroborated");
///
/// // …until a trusted signal that is not a title agrees.
/// let corroborated = MatchSignals { live_timing_agreement: true, ..alias };
/// let merged = adjudicate(Assessment { score: 0.8, signals: corroborated }, t);
/// assert_eq!(merged.verdict, MergeVerdict::Auto);
/// assert_eq!(merged.reason, "corroborated_alias");
///
/// // A numeric conflict is never merged automatically, whatever else agrees.
/// let sequel = MatchSignals { numeric_conflict: true, ..identical };
/// let apart = adjudicate(Assessment { score: 1.0, signals: sequel }, t);
/// assert_eq!(apart.verdict, MergeVerdict::Distinct);
/// assert_eq!(apart.reason, "numeric_conflict");
///
/// // A guard an operator switched on holds an otherwise-automatic merge, and names itself.
/// let remake = MatchSignals { year_conflict: true, ..identical };
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
    // These pairs are reported distinct, not queued: queueing them asks an operator to re-derive
    // the one fact the scorer is certain of.
    for (fired, veto) in [
        (
            thresholds.guards.numeric_conflict && signals.numeric_conflict,
            "numeric_conflict",
        ),
        (signals.edition_conflict, "edition_conflict"),
    ] {
        if fired {
            return Adjudication {
                verdict: MergeVerdict::Distinct,
                reason: veto,
                blocked_by: vec![veto],
            };
        }
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
    // Trusted evidence that the two are different works, against titles that say otherwise:
    // exactly the pair an operator should look at.
    for (fired, veto) in [
        (signals.external_id_conflict, "external_id_conflict"),
        (signals.live_timing_conflict, "live_timing_conflict"),
    ] {
        if fired {
            return Adjudication {
                verdict: MergeVerdict::Review,
                reason: veto,
                blocked_by: std::iter::once(veto).chain(blocked_by).collect(),
            };
        }
    }

    let merged = if signals.is_primary_identity() {
        if score < thresholds.auto_merge {
            return verdict(MergeVerdict::Review, "below_auto_merge_threshold");
        }
        "structural_identity_above_threshold"
    } else if signals.is_corroborated_alias() {
        // No score bar: the alias discount caps an alias reading at 0.80 by design, and what
        // makes this pair certain is the corroboration, not the number.
        "corroborated_alias"
    } else if signals.alias_identity {
        return verdict(MergeVerdict::Review, "alias_uncorroborated");
    } else {
        return verdict(MergeVerdict::Review, "no_structural_identity");
    };
    if let Some(guard) = blocked_by.first().copied() {
        return verdict(MergeVerdict::Review, guard);
    }
    verdict(MergeVerdict::Auto, merged)
}

/// Decides how to canonicalise the query given its candidates and thresholds.
///
/// Only a primary identity at or above [`Thresholds::high`] attaches: the incoming title equals
/// the candidate's canonical title or one of its listing titles under an identity rule. Anything
/// else at or above [`Thresholds::low`] creates the series and queues the pair for the sweep,
/// which decides with both sides' full facts. Below `low`, or with a numeric conflict, a new
/// series is created and nothing is queued.
///
/// A fuzzy score never attaches, however high: trigram and edit distance cannot tell
/// `Onikakushi-hen` from `Onisarashi-hen` or `October` from `November`. An alias hit alone does
/// not attach either, and any veto (numeric, edition, external id, live timing) refuses it.
///
/// Only the best candidate is considered (see [`best_assessment`]), so a long candidate list
/// cannot outvote it.
///
/// The same work listed by a second provider attaches:
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
///     // A poor trigram score still attaches: the scorer takes the strongest of the trigram
///     // similarity, a token-set ratio and a compact comparison, and identical titles agree
///     // completely under the last two.
///     similarity: 0.2,
///     alt_normalized_titles: Vec::new(),
///     source_titles: Vec::new(),
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
/// Without an identity the score only chooses between review and a new series. Here the titles
/// are unrelated and every other signal is absent, so the score is the trigram similarity:
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
///     source_titles: Vec::new(),
///     content_type: ContentType::Unknown,
///     release_year: None,
///     tags: Vec::new(),
///     authors: Vec::new(),
/// };
///
/// // A high fuzzy score is a question for the sweep, not an attach.
/// assert!(matches!(
///     decide(&query, &[scoring(0.9)], Thresholds::default()),
///     Decision::Ambiguous { candidate, .. } if candidate == existing
/// ));
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
    let Some((id, a)) = best_assessment(query, candidates) else {
        return Decision::Create;
    };
    // The numeric penalty in `assess` is sized to drop a sequel pair out of the bands, not
    // guaranteed to. Filing a sequel's sources under its predecessor is silent and expensive to
    // undo, so the veto is checked again here.
    if a.signals.numeric_conflict {
        return Decision::Create;
    }
    if a.signals.is_primary_identity() && !has_veto(a.signals) && a.score >= thresholds.high {
        return Decision::Attach(id);
    }
    if a.score >= thresholds.low {
        return Decision::Ambiguous {
            candidate: id,
            score: a.score,
            signals: a.signals,
        };
    }
    Decision::Create
}

/// Returns whether any signal fired that says the two are different works.
pub(crate) const fn has_veto(signals: MatchSignals) -> bool {
    signals.numeric_conflict
        || signals.edition_conflict
        || signals.external_id_conflict
        || signals.live_timing_conflict
}
