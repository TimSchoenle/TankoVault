//! The scorer proper: turning a query and one candidate into a score, its signals, and — when
//! asked — the itemised account of how the score was reached.

use tankovault_domain::matching::{Candidate, MatchSignals, Query};
use tankovault_domain::{ContentType, compact_key};

use crate::similarity::{name_set_overlap, numeric_signatures_agree, shares_a_name};
use crate::title::best_title_match;
use crate::types::{Assessment, Explanation, ScoreTerm};

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
/// merge anything automatically without [`MatchSignals::is_structural`]. A caller that only
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
/// auditable rather than merely logged: a score of 1.07-clamped-to-1.0 made of a 0.42 trigram
/// base, an exact-title bonus and a shared author reads very differently from the same number
/// made of a 0.97 base and nothing else.
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
    signals.exact_title = title_match.exact && !title_match.via_alias;
    signals.compact_identity = title_match.compact_equal && !title_match.via_alias;
    signals.alias_identity =
        (title_match.exact || title_match.compact_equal) && title_match.via_alias;
    signals.near_identical =
        !title_match.exact && !title_match.compact_equal && title_match.edit_ratio >= 0.9;
    signals.containment = title_match.containment;

    // Different numbers mean different works. Checked against *every* title the candidate
    // answers to, so a series whose alternative title carries the same volume number is not
    // penalised for its canonical title omitting it.
    signals.numeric_conflict = !numeric_signatures_agree(&query_compact, candidate);

    // Base similarity: the strongest of the trigram score (from the db), the token-set ratio
    // and the compact comparison — each catches a failure the others miss.
    let base = candidate.similarity.max(title_match.ratio);
    let mut s = base;
    ledger.term("base_similarity", base, || {
        format!(
            "strongest of trigram {:.3} and textual {:.3}",
            candidate.similarity, title_match.ratio
        )
    });

    // Content-type agreement is a strong signal (a manhwa vs. manga split matters).
    if query.content_type != ContentType::Unknown && candidate.content_type != ContentType::Unknown
    {
        if query.content_type == candidate.content_type {
            signals.type_agreement = true;
            s += 0.08;
            ledger.term("type_agreement", 0.08, || {
                format!("both {}", candidate.content_type.as_str())
            });
        } else {
            signals.type_conflict = true;
            s -= 0.15;
            ledger.term("type_conflict", -0.15, || {
                format!(
                    "{} against {}",
                    query.content_type.as_str(),
                    candidate.content_type.as_str()
                )
            });
        }
    }

    // Release-year proximity. Saturating, not `(a - b).abs()`: `release_year` is an unvalidated
    // `i32` on `GET /v1/admin/sync/suggest`, and `i32::MIN - i32::MAX` overflowed — a panic in
    // debug *and*, since SEC-11 turned on `overflow-checks` for release, in production too. See
    // `tests/prop_scoring.rs::score_survives_the_extremes_of_the_release_year_range`.
    if let (Some(a), Some(b)) = (query.release_year, candidate.release_year) {
        let gap = a.saturating_sub(b).saturating_abs();
        match gap {
            0 => {
                signals.year_agreement = true;
                s += 0.06;
                ledger.term("year_agreement", 0.06, || format!("both {a}"));
            }
            1 => {
                signals.year_agreement = true;
                s += 0.03;
                ledger.term("year_agreement", 0.03, || format!("{a} against {b}"));
            }
            d if d >= 3 => {
                signals.year_conflict = true;
                s -= 0.05;
                ledger.term("year_conflict", -0.05, || {
                    format!("{a} against {b}, {d} years apart")
                });
            }
            _ => {}
        }
    }

    // Exact normalized-title equality is decisive.
    if title_match.exact || title_match.compact_equal {
        s += 0.1;
        ledger.term("title_identity", 0.1, || {
            let rule = if title_match.exact {
                "identical"
            } else {
                "identical ignoring whitespace"
            };
            let side = if title_match.via_alias {
                "an alternative title"
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

    // Genre/tag overlap. A weak signal on its own (genres are coarse and inconsistently
    // tagged across sites), so the bonus is small and scales with how much the two sets
    // actually agree rather than firing at full strength on a single shared genre.
    if let Some(overlap) = name_set_overlap(&query.tags, &candidate.tags) {
        signals.tag_overlap = overlap > 0.0;
        s += 0.05 * overlap;
        ledger.term("tag_overlap", 0.05 * overlap, || {
            format!("{:.0}% of the two tag sets agree", overlap * 100.0)
        });
    }

    // A shared author/artist credit is a strong, low-false-positive signal: two unrelated
    // works with a similar title essentially never share a real person's name too.
    if shares_a_name(&query.authors, &candidate.authors) {
        signals.shared_author = true;
        s += 0.1;
        ledger.term("shared_author", 0.1, || {
            format!("credits agree on at least one of {:?}", query.authors)
        });
    } else if !query.authors.is_empty() && !candidate.authors.is_empty() {
        // No score term: disagreeing credits are a *guard*, not a penalty. Sizing a penalty
        // that reliably drops an exact-title pair below the automatic threshold would also
        // drop honest matches out of the review band, where they belong. `adjudicate` blocks
        // on the signal instead, which holds the pair for an operator without moving its score.
        signals.author_conflict = true;
        ledger.term("author_conflict", 0.0, || {
            format!(
                "{:?} against {:?}, none in common",
                query.authors, candidate.authors
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
                "{:?} carries different numbers from every title of the candidate",
                query.normalized_title
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
