//! The scorer: one query and one candidate in, a score and its signals out, with the itemised
//! account of each term when the caller asks for it.

use tankovault_domain::matching::{Candidate, MatchSignals, Query};
use tankovault_domain::{ContentType, alias_is_distinctive, compact_key};

use crate::edition::editions_conflict;
use crate::similarity::{
    is_strict_token_subset, name_set_overlap, numbers_conflict, shares_a_name,
};
use crate::title::{TitleMatch, best_title_match};
use crate::types::{Assessment, Explanation, ScoreTerm};

/// The weight of a title agreement against an alternative title, relative to one against a
/// primary title.
///
/// Sized against [`Thresholds`](crate::Thresholds): a perfect alias hit scores
/// `1.0 * 0.75 + 0.05 = 0.80`, inside the review band `[low, high)`. No other term lifts it out,
/// since medium, year and credits are never weighed, and [`decide`](crate::decide) and
/// [`adjudicate`](crate::adjudicate) refuse an alias on its own whatever it scores. The discount
/// orders the review queue.
const ALIAS_EVIDENCE_WEIGHT: f32 = 0.75;

/// Collects the terms of a score, or discards them.
///
/// One scorer serves two callers. [`assess`] wants only the number and runs on the ingest path;
/// [`explain`] wants the whole account and runs once per recorded decision. A separate scorer for
/// explaining would drift from this one, and the drift would be invisible because the
/// explanation is what an operator checks the score with. The detail string is built behind a
/// closure, so the discarding case never formats it.
struct Ledger(Option<Vec<ScoreTerm>>);

impl Ledger {
    const fn discarding() -> Self {
        Self(None)
    }

    const fn collecting() -> Self {
        Self(Some(Vec::new()))
    }

    fn term(&mut self, rule: &'static str, delta: f32, detail: impl FnOnce() -> String) {
        if let Some(terms) = self.0.as_mut() {
            terms.push(ScoreTerm {
                rule,
                delta,
                detail: detail(),
            });
        }
    }
}

/// Scores a single candidate against the query, capped at 1.0.
#[must_use]
pub fn score(query: &Query, candidate: &Candidate) -> f32 {
    assess(query, candidate).score
}

/// Scores a candidate and reports which rules produced the score.
///
/// The signals are part of the contract. [`Decision::Ambiguous`](crate::Decision::Ambiguous)
/// carries them into the merge queue's stored reason, the operator console renders them as
/// badges, and [`adjudicate`](crate::adjudicate) merges nothing automatically without
/// [`MatchSignals::is_primary_identity`] or [`MatchSignals::is_corroborated_alias`]. Use
/// [`score`] for the number alone and [`explain`] to justify a destructive action afterwards.
#[must_use]
pub fn assess(query: &Query, candidate: &Candidate) -> Assessment {
    assess_with(query, candidate, &mut Ledger::discarding()).assessment
}

/// Scores a candidate and keeps every term that produced the score.
///
/// The arithmetic is the function [`assess`] runs, with the running total itemised. A merge
/// decision record stores it, so an automatic merge can be audited: a 1.0 made of a 0.42 trigram
/// base and an exact-title bonus reads differently from a 1.0 made of a 0.97 base alone.
#[must_use]
pub fn explain(query: &Query, candidate: &Candidate) -> Explanation {
    let mut ledger = Ledger::collecting();
    let scored = assess_with(query, candidate, &mut ledger);
    Explanation {
        assessment: scored.assessment,
        base: scored.base,
        terms: ledger.0.unwrap_or_default(),
        matched_query_title: query.normalized_title.clone(),
        matched_candidate_title: scored.matched.to_owned(),
        via_alias: scored.via_alias,
    }
}

/// The scorer's full return: the assessment plus the two facts an explanation needs and a bare
/// score does not. `matched` borrows from the candidate, so the discarding path allocates nothing.
struct Scored<'a> {
    assessment: Assessment,
    base: f32,
    /// The candidate title the winning comparison was against.
    matched: &'a str,
    /// Whether that title was one of the candidate's alternative titles.
    via_alias: bool,
}

/// Scores `candidate` against `query`, recording each term in `ledger`.
///
/// Each step records its signals together with the terms that explain them. The steps run in a
/// fixed order, which is the order an explanation lists its terms in.
fn assess_with<'a>(query: &Query, candidate: &'a Candidate, ledger: &mut Ledger) -> Scored<'a> {
    let query_compact = compact_key(&query.normalized_title);
    let title_match = best_title_match(&query.normalized_title, &query_compact, candidate);
    let mut signals = title_signals(query, candidate, &title_match);
    let base = base_similarity(candidate, &title_match, ledger);
    let mut s = base;
    record_medium_and_year(query, candidate, &mut signals, ledger);
    s += title_bonus(&title_match, ledger);
    record_tags_and_credits(query, candidate, &mut signals, ledger);
    if record_conflicts(query, candidate, &title_match, &signals, ledger) {
        s -= NUMERIC_CONFLICT_PENALTY;
    }

    Scored {
        assessment: Assessment {
            score: s.clamp(0.0, 1.0),
            signals,
        },
        base,
        matched: title_match.matched,
        via_alias: title_match.via_alias,
    }
}

/// What a numeric conflict takes off the score.
const NUMERIC_CONFLICT_PENALTY: f32 = 0.3;

/// Returns the signals the title comparison alone decides.
fn title_signals(
    query: &Query,
    candidate: &Candidate,
    title_match: &TitleMatch<'_>,
) -> MatchSignals {
    let mut signals = MatchSignals::default();
    let canonical_hit = !title_match.via_alias && !title_match.via_source;
    signals.exact_title = title_match.exact && canonical_hit;
    signals.compact_identity = title_match.compact_equal && canonical_hit;
    let identical = title_match.exact || title_match.compact_equal;
    signals.source_title_identity = identical && title_match.via_source;
    let alias_hit = identical && title_match.via_alias;
    signals.alias_identity =
        alias_hit && alias_counts_as_identity(title_match.matched, &[&candidate.normalized_title]);
    signals.near_identical =
        !title_match.exact && !title_match.compact_equal && title_match.edit_ratio >= 0.9;
    signals.containment = title_match.containment
        || (alias_hit && is_franchise_name(title_match.matched, &candidate.normalized_title));

    // Different numbers or a different edition mean different works. Both the title the query
    // was scored against and the candidate's canonical title are checked: a conflict with the
    // canonical title stands even when an alias agrees.
    let own_titles = [title_match.matched, candidate.normalized_title.as_str()];
    signals.numeric_conflict = numbers_conflict(&query.normalized_title, &own_titles);
    signals.edition_conflict = own_titles
        .iter()
        .any(|t| editions_conflict(&query.normalized_title, t));

    signals
}

/// Returns the base similarity, discounted when the best agreement was against an alternative
/// title.
fn base_similarity(
    candidate: &Candidate,
    title_match: &TitleMatch<'_>,
    ledger: &mut Ledger,
) -> f32 {
    // The strongest of the database's trigram score, the token-set ratio and the compact
    // comparison. Each catches a failure the others miss.
    let textual = candidate.similarity.max(title_match.ratio);

    // An exact hit on an alternative title is evidence, not identity, and must not clear
    // `Thresholds::high`. Aliases are scraped free text: they arrive split out of comma-joined
    // cells and are shared across the `(Colored)` and franchise listings of unrelated works.
    // Attaching a source also files its aliases under the series, so one junk alias that
    // attracts one wrong series then attracts everything that series answers to.
    //
    // The trigram score is discounted too. `find_candidates` reports the maximum similarity over
    // the canonical title and every alias, so a perfect alias hit arrives as 1.0 and would
    // defeat the discount on its own.
    let base = if title_match.via_alias {
        (textual * ALIAS_EVIDENCE_WEIGHT).max(title_match.primary_ratio)
    } else {
        textual
    };
    ledger.term("base_similarity", base, || {
        let via = if title_match.via_alias {
            format!(", discounted to {ALIAS_EVIDENCE_WEIGHT} as an alternative title")
        } else {
            String::new()
        };
        format!(
            "strongest of trigram {:.3} and textual {:.3}{via}",
            candidate.similarity, title_match.ratio
        )
    });
    base
}

/// Records the medium and year signals, each explained and never weighed.
fn record_medium_and_year(
    query: &Query,
    candidate: &Candidate,
    signals: &mut MatchSignals,
    ledger: &mut Ledger,
) {
    // Medium, year, tags and credits disagree across providers too often to be weighed, and any
    // weight they carried would lift a discounted alias hit back over the attach and merge
    // thresholds. A zero-delta term still shows the operator what was seen.
    if query.content_type != ContentType::Unknown && candidate.content_type != ContentType::Unknown
    {
        if query.content_type == candidate.content_type {
            signals.type_agreement = true;
            ledger.term("type_agreement", 0.0, || {
                format!("both {}", candidate.content_type.as_str())
            });
        } else {
            signals.type_conflict = true;
            ledger.term("type_conflict", 0.0, || {
                format!(
                    "{} against {}",
                    query.content_type.as_str(),
                    candidate.content_type.as_str()
                )
            });
        }
    }

    // Saturating, not `(a - b).abs()`: `release_year` is an unvalidated `i32` on
    // `GET /v1/admin/sync/suggest`, and release builds keep overflow checks, so the plain
    // subtraction panics at the extremes of the range.
    if let (Some(a), Some(b)) = (query.release_year, candidate.release_year) {
        let gap = a.saturating_sub(b).saturating_abs();
        if gap <= 1 {
            signals.year_agreement = true;
            ledger.term("year_agreement", 0.0, || format!("{a} against {b}"));
        } else if gap >= 3 {
            signals.year_conflict = true;
            ledger.term("year_conflict", 0.0, || {
                format!("{a} against {b}, {gap} years apart")
            });
        }
    }
}

/// Returns the bonus a title identity or containment earns, or 0 when there is neither.
fn title_bonus(title_match: &TitleMatch<'_>, ledger: &mut Ledger) -> f32 {
    // Exact equality with a primary title is decisive. With an alternative title it earns half
    // the bonus, for the reason given at the alias discount.
    if title_match.exact || title_match.compact_equal {
        let bonus = if title_match.via_alias { 0.05 } else { 0.1 };
        ledger.term("title_identity", bonus, || {
            let rule = if title_match.exact {
                "identical"
            } else {
                "identical ignoring whitespace"
            };
            let side = if title_match.via_alias {
                "an alternative title"
            } else if title_match.via_source {
                "a source's listing title"
            } else {
                "the canonical title"
            };
            format!("{rule}, against {side} {:?}", title_match.matched)
        });
        bonus
    } else if title_match.containment {
        // One title's words are a subset of the other's, as with an abbreviated and a fully
        // subtitled listing of one work. The nudge is smaller than the identity bonus because a
        // sequel's title often contains its predecessor's.
        let bonus = 0.05;
        ledger.term("containment", bonus, || {
            format!(
                "every word of one title appears in {:?}",
                title_match.matched
            )
        });
        bonus
    } else {
        0.0
    }
}

/// Returns the tag and credit signals. Each is recorded and explained, and never weighed.
fn record_tags_and_credits(
    query: &Query,
    candidate: &Candidate,
    signals: &mut MatchSignals,
    ledger: &mut Ledger,
) {
    if let Some(overlap) = name_set_overlap(&query.tags, &candidate.tags) {
        signals.tag_overlap = overlap > 0.0;
        ledger.term("tag_overlap", 0.0, || {
            format!("{:.0}% of the two tag sets agree", overlap * 100.0)
        });
    }

    if shares_a_name(&query.authors, &candidate.authors) {
        signals.shared_author = true;
        ledger.term("shared_author", 0.0, || {
            format!("credits agree on at least one of {:?}", query.authors)
        });
    } else if !query.authors.is_empty() && !candidate.authors.is_empty() {
        signals.author_conflict = true;
        ledger.term("author_conflict", 0.0, || {
            format!(
                "{:?} against {:?}, none in common",
                query.authors, candidate.authors
            )
        });
    }
}

/// Records the edition and numeric conflict terms, and returns whether the numeric veto applies.
fn record_conflicts(
    query: &Query,
    candidate: &Candidate,
    title_match: &TitleMatch<'_>,
    signals: &MatchSignals,
    ledger: &mut Ledger,
) -> bool {
    if signals.edition_conflict {
        ledger.term("edition_conflict", 0.0, || {
            format!(
                "{:?} names a different edition or arc from {:?}",
                query.normalized_title, candidate.normalized_title
            )
        });
    }

    // Applied last, so it bites whatever the rest of the scorer produced. A sequel's title
    // resembles its predecessor's more closely the better the matcher works, so this term must
    // overcome a near-perfect fuzzy score. It drops a 0.89 pair (`overlord` against `overlord2`)
    // below the review floor, and [`decide`] refuses the attach whatever the arithmetic says.
    if signals.numeric_conflict {
        ledger.term("numeric_conflict", -NUMERIC_CONFLICT_PENALTY, || {
            format!(
                "{:?} carries different numbers from {:?} or {:?}",
                query.normalized_title, title_match.matched, candidate.normalized_title
            )
        });
    }
    signals.numeric_conflict
}

/// The most words an alias may have and still read as a franchise name when another title
/// contains it.
///
/// A longer alias is the unsubtitled form of one long title, such as the light novel
/// `I'll Create the Ultimate Golem Together with My Beautiful Hero Disciples…` inside its
/// subtitled listing, and names the same work.
const MAX_FRANCHISE_WORDS: usize = 4;

/// Returns whether `alias` names the franchise `canonical` belongs to: every word of it appears
/// in `canonical`, which says more (`fairy tail` against `fairy tail 100 years quest`).
pub(crate) fn is_franchise_name(alias: &str, canonical: &str) -> bool {
    alias.split_whitespace().nth(MAX_FRANCHISE_WORDS).is_none()
        && is_strict_token_subset(alias, canonical)
}

/// Returns whether an identity hit on `alias` counts as `alias_identity`.
///
/// The alias must be distinctive ([`alias_is_distinctive`]) and must not be a franchise name of
/// any of `canonicals`. A franchise hit is containment, not identity: `sao`, `fairy tail` and
/// `tokyo ghoul` each name several works.
pub(crate) fn alias_counts_as_identity(alias: &str, canonicals: &[&str]) -> bool {
    alias_is_distinctive(alias) && !canonicals.iter().any(|c| is_franchise_name(alias, c))
}
