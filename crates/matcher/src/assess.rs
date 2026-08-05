//! The scorer proper: turning a query and one candidate into a score and its signals.

use tankovault_domain::matching::{Candidate, MatchSignals, Query};
use tankovault_domain::{ContentType, compact_key};

use crate::similarity::{name_set_overlap, numeric_signatures_agree, shares_a_name};
use crate::title::best_title_match;
use crate::types::Assessment;

/// Score a single candidate against the query, capped at 1.0.
#[must_use]
pub fn score(query: &Query, candidate: &Candidate) -> f32 {
    assess(query, candidate).score
}

/// Score a candidate **and** report which rules produced the score.
///
/// The signals are not diagnostics: [`Decision::Ambiguous`] carries them into the merge queue's
/// stored reason, the operator console renders them as badges, and [`adjudicate`] refuses to
/// merge anything automatically without [`MatchSignals::is_structural`]. A caller that only
/// wants the number can use [`score`].
#[must_use]
pub fn assess(query: &Query, candidate: &Candidate) -> Assessment {
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
    let mut s = candidate.similarity.max(title_match.ratio);

    // Content-type agreement is a strong signal (a manhwa vs. manga split matters).
    if query.content_type != ContentType::Unknown && candidate.content_type != ContentType::Unknown
    {
        if query.content_type == candidate.content_type {
            signals.type_agreement = true;
            s += 0.08;
        } else {
            signals.type_conflict = true;
            s -= 0.15;
        }
    }

    // Release-year proximity. Saturating, not `(a - b).abs()`: `release_year` is an unvalidated
    // `i32` on `GET /v1/admin/sync/suggest`, and `i32::MIN - i32::MAX` overflowed — a panic in
    // debug *and*, since SEC-11 turned on `overflow-checks` for release, in production too. See
    // `tests/prop_scoring.rs::score_survives_the_extremes_of_the_release_year_range`.
    if let (Some(a), Some(b)) = (query.release_year, candidate.release_year) {
        match a.saturating_sub(b).saturating_abs() {
            0 => {
                signals.year_agreement = true;
                s += 0.06;
            }
            1 => {
                signals.year_agreement = true;
                s += 0.03;
            }
            d if d >= 3 => {
                signals.year_conflict = true;
                s -= 0.05;
            }
            _ => {}
        }
    }

    // Exact normalized-title equality is decisive.
    if title_match.exact || title_match.compact_equal {
        s += 0.1;
    } else if title_match.containment {
        // One title's words are a full subset of the other's (e.g. an abbreviated vs. a
        // fully-subtitled edition of the same work). A modest nudge, deliberately smaller
        // than the exact-match bonus so genuine sequels don't get over-attached.
        s += 0.05;
    }

    // Genre/tag overlap. A weak signal on its own (genres are coarse and inconsistently
    // tagged across sites), so the bonus is small and scales with how much the two sets
    // actually agree rather than firing at full strength on a single shared genre.
    if let Some(overlap) = name_set_overlap(&query.tags, &candidate.tags) {
        signals.tag_overlap = overlap > 0.0;
        s += 0.05 * overlap;
    }

    // A shared author/artist credit is a strong, low-false-positive signal: two unrelated
    // works with a similar title essentially never share a real person's name too.
    if shares_a_name(&query.authors, &candidate.authors) {
        signals.shared_author = true;
        s += 0.1;
    }

    // The veto, applied last so it bites whatever the rest of the scorer produced. A sequel's
    // title is *more* similar to its predecessor's the better the matcher works, so this is the
    // one term that has to be able to overcome a near-perfect fuzzy score. It is deliberately
    // large enough to drop a 0.89 pair (`overlord` against `overlord2`) below the create
    // threshold rather than merely out of the attach band, and [`decide`] refuses to attach on
    // it regardless of the arithmetic.
    if signals.numeric_conflict {
        s -= 0.3;
    }

    Assessment {
        score: s.clamp(0.0, 1.0),
        signals,
    }
}
