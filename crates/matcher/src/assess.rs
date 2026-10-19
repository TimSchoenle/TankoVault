//! The scorer proper: turning a query and one candidate into a score, its signals, and — when
//! asked — the itemised account of how the score was reached.

use tankovault_domain::matching::{Candidate, MatchSignals, Query};
use tankovault_domain::{ContentType, alias_is_distinctive, compact_key};

use crate::edition::editions_conflict;
use crate::similarity::{
    is_strict_token_subset, name_set_overlap, numbers_conflict, shares_a_name,
};
use crate::title::best_title_match;
use crate::types::{Assessment, Explanation, ScoreTerm};

/// What a title agreement is worth when it is against an *alternative* title rather than the
/// canonical one.
///
/// Sized against [`Thresholds`](crate::Thresholds): a perfect alias hit scores
/// `1.0 * 0.75 + 0.05 = 0.80`, inside the review band `[low, high)`. Nothing lifts it back out:
/// the medium, year and credit bonuses that used to are gone (see [`assess_with`]), and
/// [`decide`](crate::decide) and [`adjudicate`](crate::adjudicate) refuse an alias on its own
/// whatever it scores. The discount still orders the review queue.
const ALIAS_EVIDENCE_WEIGHT: f32 = 0.75;

/// Collects the terms of a score, or discards them.
///
/// One scorer, two callers: [`assess`] wants only the number and runs on the ingest path, while
/// [`explain`] wants the whole account and runs once per recorded decision. A second
/// implementation for the explaining case is the obvious alternative and the wrong one — the two
/// would drift, and the drift would be invisible precisely because the explanation is what an
/// operator checks the score *with*. The detail string is built behind a closure, so the
/// discarding case does not format it.
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

/// Score a single candidate against the query, capped at 1.0.
#[must_use]
pub fn score(query: &Query, candidate: &Candidate) -> f32 {
    assess(query, candidate).score
}

/// Score a candidate **and** report which rules produced the score.
///
/// The signals are not diagnostics: [`Decision::Ambiguous`](crate::Decision::Ambiguous) carries them into the merge queue's
/// stored reason, the operator console renders them as badges, and [`adjudicate`](crate::adjudicate) refuses to
/// merge anything automatically without [`MatchSignals::is_primary_identity`] or
/// [`MatchSignals::is_corroborated_alias`]. A caller that only
/// wants the number can use [`score`]; one that needs to justify a destructive action after the
/// fact wants [`explain`].
#[must_use]
pub fn assess(query: &Query, candidate: &Candidate) -> Assessment {
    assess_with(query, candidate, &mut Ledger::discarding()).assessment
}

/// Score a candidate and keep every term that produced the score.
///
/// The same arithmetic as [`assess`] — literally the same function — with the running total
/// itemised. This is what a merge decision record stores, and what makes an automatic merge
/// auditable rather than merely logged: a score of 1.0 made of a 0.42 trigram base and an
/// exact-title bonus reads very differently from the same number made of a 0.97 base and
/// nothing else.
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
    /// Whether that title was an alternative rather than the canonical one.
    via_alias: bool,
}

/// The scorer, shared by [`assess`] and [`explain`].
#[expect(
    clippy::too_many_lines,
    reason = "one arm per scoring rule, each pairing its signal, its weight and the phrase that \
              explains it. Splitting it would separate a rule from the term it records, which is \
              exactly the drift the single-implementation design exists to prevent"
)]
fn assess_with<'a>(query: &Query, candidate: &'a Candidate, ledger: &mut Ledger) -> Scored<'a> {
    let mut signals = MatchSignals::default();

    let query_compact = compact_key(&query.normalized_title);
    let title_match = best_title_match(&query.normalized_title, &query_compact, candidate);
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

    // Different numbers or a different edition mean different works. Checked against the title
    // the query was scored against *and* the candidate's canonical title: a conflict with the
    // canonical title is a conflict even when an alias agrees.
    let own_titles = [title_match.matched, candidate.normalized_title.as_str()];
    signals.numeric_conflict = numbers_conflict(&query.normalized_title, &own_titles);
    signals.edition_conflict = own_titles
        .iter()
        .any(|t| editions_conflict(&query.normalized_title, t));

    // Base similarity: the strongest of the trigram score (from the db), the token-set ratio
    // and the compact comparison — each catches a failure the others miss.
    let textual = candidate.similarity.max(title_match.ratio);

    // An exact hit on an *alternative* title is evidence, not identity, and must not clear
    // `Thresholds::high`. Aliases are scraped free text: they arrive shredded out of a
    // comma-joined cell, and they are shared across the `(Colored)` and franchise listings of
    // unrelated works. Because attaching a source also files *its* aliases under the series it
    // attached to, one junk alias that attracts one wrong series then attracts everything that
    // series answers to — a runaway that put 281 unrelated sources on one live row.
    //
    // The trigram score is discounted with the rest, not exempted: `find_candidates` reports the
    // *maximum* similarity over the canonical title and every alias, so a perfect alias hit
    // arrives from the database as 1.0 and would otherwise defeat the discount by itself.
    let base = if title_match.via_alias {
        (textual * ALIAS_EVIDENCE_WEIGHT).max(title_match.primary_ratio)
    } else {
        textual
    };
    let mut s = base;
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

    // Medium, year, tags and credits are recorded and explained, and never weighed. Each is
    // wrong or inconsistent across providers most of the time, and each was exactly what lifted a
    // discounted alias hit back over the attach and merge thresholds: 12,356 of the automatic
    // alias merges fired a metadata guard that production had switched off for being too noisy.
    // A zero-delta term keeps the explanation honest about what was seen.
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
    // `GET /v1/admin/sync/suggest`, and `i32::MIN - i32::MAX` overflowed — a panic in debug *and*,
    // since SEC-11 turned on `overflow-checks` for release, in production too. See
    // `tests/prop_scoring.rs::score_survives_the_extremes_of_the_release_year_range`.
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

    // Exact normalized-title equality is decisive — against a primary title. Against an
    // alternative it is half a bonus, for the reason the base discount above spells out.
    if title_match.exact || title_match.compact_equal {
        let bonus = if title_match.via_alias { 0.05 } else { 0.1 };
        s += bonus;
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
    } else if title_match.containment {
        // One title's words are a full subset of the other's (e.g. an abbreviated vs. a
        // fully-subtitled edition of the same work). A modest nudge, deliberately smaller
        // than the exact-match bonus so genuine sequels don't get over-attached.
        s += 0.05;
        ledger.term("containment", 0.05, || {
            format!(
                "every word of one title appears in {:?}",
                title_match.matched
            )
        });
    }

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

    if signals.edition_conflict {
        ledger.term("edition_conflict", 0.0, || {
            format!(
                "{:?} names a different edition or arc from {:?}",
                query.normalized_title, candidate.normalized_title
            )
        });
    }

    // The veto, applied last so it bites whatever the rest of the scorer produced. A sequel's
    // title is *more* similar to its predecessor's the better the matcher works, so this is the
    // one term that has to be able to overcome a near-perfect fuzzy score. It is deliberately
    // large enough to drop a 0.89 pair (`overlord` against `overlord2`) below the create
    // threshold rather than merely out of the attach band, and [`decide`] refuses to attach on
    // it regardless of the arithmetic.
    if signals.numeric_conflict {
        s -= 0.3;
        ledger.term("numeric_conflict", -0.3, || {
            format!(
                "{:?} carries different numbers from {:?} or {:?}",
                query.normalized_title, title_match.matched, candidate.normalized_title
            )
        });
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

/// Most words an alias may have and still be read as a franchise name when another title
/// contains it. Past this it is a subtitle-less form of one long title (the light-novel
/// `I'll Create the Ultimate Golem Together with My Beautiful Hero Disciples…` inside its
/// subtitled listing), which is the same work.
const MAX_FRANCHISE_WORDS: usize = 4;

/// Whether `alias` names a franchise rather than a work, against `canonical`: every word of it
/// is in `canonical`, which says more (`fairy tail` against `fairy tail 100 years quest`).
pub(crate) fn is_franchise_name(alias: &str, canonical: &str) -> bool {
    alias.split_whitespace().nth(MAX_FRANCHISE_WORDS).is_none()
        && is_strict_token_subset(alias, canonical)
}

/// Whether an identity hit on `alias` counts as `alias_identity`: the alias must be distinctive
/// ([`alias_is_distinctive`]) and must not be a franchise name of any of `canonicals`. A
/// franchise hit is containment, not identity: `sao`, `fairy tail` and `tokyo ghoul` each joined
/// several different works of one franchise in production.
pub(crate) fn alias_counts_as_identity(alias: &str, canonicals: &[&str]) -> bool {
    alias_is_distinctive(alias) && !canonicals.iter().any(|c| is_franchise_name(alias, c))
}
