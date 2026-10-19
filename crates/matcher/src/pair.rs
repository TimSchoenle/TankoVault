//! Judging two series that already exist: score the pair every way round, keep the best reading,
//! and adjudicate it. One implementation for the duplicate sweep and the offline replay harness,
//! so a replayed verdict is the verdict the sweep would reach.

use tankovault_domain::matching::{Candidate, MatchSignals, Query};
use tankovault_domain::{ContentType, SeriesId};

use crate::assess::{alias_counts_as_identity, assess, explain, is_franchise_name};
use crate::decide::adjudicate;
use crate::edition::editions_conflict;
use crate::similarity::numbers_conflict;
use crate::types::{Adjudication, Assessment, ScoreTerm, Thresholds};

/// One side of a pair, borrowed from whatever holds the series' facts.
#[derive(Debug, Clone, Copy)]
pub struct PairSide<'a> {
    /// The series these facts describe.
    pub series_id: SeriesId,
    /// Its canonical normalized title.
    pub normalized_title: &'a str,
    /// Its alternative normalized titles.
    pub alt_normalized_titles: &'a [String],
    /// Its recorded medium.
    pub content_type: ContentType,
    /// Its recorded year.
    pub release_year: Option<i32>,
    /// Its genre names.
    pub tags: &'a [String],
    /// Its credits.
    pub authors: &'a [String],
}

/// The winning reading of a pair: which title on which side was scored against which, the
/// resulting assessment, and the itemised terms behind it.
#[derive(Debug, Clone)]
pub struct PairReading {
    /// The verdict-bearing score and signals.
    pub assessment: Assessment,
    /// The base similarity before any term.
    pub base: f32,
    /// Every term applied, in order.
    pub terms: Vec<ScoreTerm>,
    /// The query-side title of the winning reading.
    pub matched_query_title: String,
    /// The candidate-side title it won against.
    pub matched_candidate_title: String,
    /// Whether the *candidate* side's matching title was an alternative.
    pub via_alias: bool,
    /// Whether the query side of the winning reading was the left series.
    pub query_is_left: bool,
}

/// Read the pair with [`read_pair`] and adjudicate the winning reading.
#[must_use]
pub fn adjudicate_pair(
    left: PairSide<'_>,
    right: PairSide<'_>,
    trigram: f32,
    thresholds: Thresholds,
) -> (PairReading, Adjudication) {
    let reading = read_pair(left, right, trigram);
    let verdict = adjudicate(reading.assessment, thresholds);
    (reading, verdict)
}

/// Score the pair every way round and keep the best reading, explained.
///
/// # Why both directions, and every alias
///
/// `Query` carries no alternative titles, so `assess(left, right)` sees only the right side's
/// synonyms. Correct for a scan — the incoming source genuinely has one title — but here both
/// sides have synonym lists, and scoring one direction only misses the duplicate whose shared
/// name sits on the *query* side. Two rounds of canonical-as-query cover canonical-against-
/// canonical and canonical-against-alias but never compare two aliases, which is the third of
/// `find_duplicate_pairs`' three collisions: 438 such pairs sat permanently in the review queue
/// because no structural signal could fire for them.
///
/// # Why so little is built inside the loop
///
/// A pair with `k` aliases a side is `2(k+1)` readings, and the only thing that varies between
/// readings of one direction is the query's title. [`assess`] itself allocates nothing, but a
/// [`Candidate`] and a [`Query`] built per reading clone both sides' alias, tag and author lists
/// each time — which made the search allocate in proportion to the square of a pair's synonyms
/// for values it then threw away. Both are built once per direction.
///
/// # Why the winner is re-scored
///
/// The search runs on [`assess`], and only the winner is re-run through [`explain`] to itemise
/// it. Explaining every reading would allocate a term list per alias per direction for a number
/// the sweep discards.
///
/// # Why an identity reading outranks a higher-scoring fuzzy one
///
/// `trigram` is the *pair's* similarity, and `pair_similarities` takes it over each side's
/// alternative titles as well as the two canonical ones — so a pair whose synonym identifies it
/// hands every reading a number the canonical titles did not earn. `assess` bases a reading on
/// `max(trigram, textual)`, which lifts the canonical reading to the clamp ceiling with no
/// identity signal behind it, where it ties with the alias reading that *does* carry one. Ranked
/// on score alone, the tie went to whichever came first — the canonical one — and the pair was
/// filed as `no_structural_identity` at 100%, with no badge saying which title the number came
/// from. So readings rank by the strength of their claim, then by score: a primary identity, a
/// corroborated alias, an alias on its own, nothing. The class is read *after* the pair rules
/// below, so a query-side synonym hit ranks as the alias hit it is.
///
/// # The rules only a pair can apply
///
/// `assess` sees one query title and the candidate's titles. Here both sides have a canonical
/// title of their own, so a reading also has to agree with the *query side's* canonical title:
/// its numbers and edition markers, and its franchise name. Douluo Dalu 4 merged into 5 because
/// the reading compared the alias `douluo dalu 4 zhongji douluo` with itself and never looked at
/// `Douluo Dalu 5`.
///
/// # Panics
///
/// Never: every pair has at least its two canonical readings.
#[must_use]
pub fn read_pair(left: PairSide<'_>, right: PairSide<'_>, trigram: f32) -> PairReading {
    // the reading's class and score, whether the query side was the left series, the winning
    // title, whether it is an alias
    let mut best: Option<((u8, f32), bool, &str, bool)> = None;

    for query_is_left in [true, false] {
        let (query_side, candidate_side) = if query_is_left {
            (left, right)
        } else {
            (right, left)
        };
        let candidate = candidate_of(candidate_side, trigram);
        let mut query = query_of(query_side, String::new());

        // The canonical title goes first and ties are kept (the comparison is strict), so a pair
        // that both a canonical title and an alias explain *in the same class* is reported with
        // the canonical reading's signals.
        let titles = std::iter::once((query_side.normalized_title, false)).chain(
            query_side
                .alt_normalized_titles
                .iter()
                .map(|alias| (alias.as_str(), true)),
        );
        for (title, from_alias) in titles {
            query.normalized_title.clear();
            query.normalized_title.push_str(title);
            let assessment = assess(&query, &candidate);
            let signals = pair_rules(
                assessment.signals,
                query_side,
                candidate_side,
                title,
                from_alias,
            );
            let rank = (class(signals), assessment.score);
            if best.is_none_or(|(best_rank, ..)| rank > best_rank) {
                best = Some((rank, query_is_left, title, from_alias));
            }
        }
    }

    let (_, query_is_left, title, from_alias) =
        best.expect("a pair always has at least the two canonical readings");
    let (query_side, candidate_side) = if query_is_left {
        (left, right)
    } else {
        (right, left)
    };
    let query = query_of(query_side, title.to_owned());
    let candidate = candidate_of(candidate_side, trigram);
    let mut explained = explain(&query, &candidate);
    let scored = explained.assessment.signals;
    let signals = pair_rules(scored, query_side, candidate_side, title, from_alias);
    let canonicals = || {
        format!(
            "the canonical titles {:?} and {:?}",
            query_side.normalized_title, candidate_side.normalized_title
        )
    };
    // Zero-delta terms: the pair rules veto rather than score, and the terms must still sum to
    // the score.
    if signals.numeric_conflict && !scored.numeric_conflict {
        explained.terms.push(ScoreTerm {
            rule: "numeric_conflict",
            delta: 0.0,
            detail: format!("{} carry different numbers", canonicals()),
        });
    }
    if signals.edition_conflict && !scored.edition_conflict {
        explained.terms.push(ScoreTerm {
            rule: "edition_conflict",
            delta: 0.0,
            detail: format!("{} name different editions or arcs", canonicals()),
        });
    }
    explained.assessment.signals = signals;

    PairReading {
        assessment: explained.assessment,
        base: explained.base,
        terms: explained.terms,
        matched_query_title: title.to_owned(),
        matched_candidate_title: explained.matched_candidate_title,
        via_alias: explained.via_alias,
        query_is_left,
    }
}

fn query_of(side: PairSide<'_>, normalized_title: String) -> Query {
    Query {
        normalized_title,
        content_type: side.content_type,
        release_year: side.release_year,
        tags: side.tags.to_vec(),
        authors: side.authors.to_vec(),
    }
}

fn candidate_of(side: PairSide<'_>, similarity: f32) -> Candidate {
    Candidate {
        series_id: side.series_id,
        normalized_title: side.normalized_title.to_owned(),
        similarity,
        alt_normalized_titles: side.alt_normalized_titles.to_vec(),
        content_type: side.content_type,
        release_year: side.release_year,
        tags: side.tags.to_vec(),
        authors: side.authors.to_vec(),
    }
}

/// How strong a reading's claim is: a primary identity, a corroborated alias, an alias, nothing.
const fn class(signals: MatchSignals) -> u8 {
    if signals.is_primary_identity() {
        3
    } else if signals.is_corroborated_alias() {
        2
    } else if signals.alias_identity {
        1
    } else {
        0
    }
}

/// Apply the rules that need both sides' canonical titles to one reading's signals.
///
/// An identity hit through one of the *query* side's alternative titles is relabelled:
/// `exact_title` and `compact_identity` assert that the two **canonical** titles are the same
/// string, which is what the console renders as a badge, what the auto-merge journal records as
/// its justification for deleting a row, and what [`adjudicate`] merges on. Through a synonym it
/// is an alias hit, held to the alias rules.
fn pair_rules(
    mut signals: MatchSignals,
    query_side: PairSide<'_>,
    candidate_side: PairSide<'_>,
    query_title: &str,
    query_from_alias: bool,
) -> MatchSignals {
    let canonicals = [query_side.normalized_title, candidate_side.normalized_title];
    let alias_hit = if query_from_alias && (signals.exact_title || signals.compact_identity) {
        signals.exact_title = false;
        signals.compact_identity = false;
        true
    } else {
        signals.alias_identity
    };
    if alias_hit {
        // `assess` already held a candidate-side alias to the candidate's canonical title; this
        // adds the query side's, and covers a query-side alias outright.
        signals.alias_identity = alias_counts_as_identity(query_title, &canonicals);
        signals.containment |= canonicals.iter().any(|c| is_franchise_name(query_title, c));
    }
    signals.numeric_conflict |= numbers_conflict(canonicals[0], &canonicals[1..]);
    signals.edition_conflict |= editions_conflict(canonicals[0], canonicals[1]);
    signals
}
