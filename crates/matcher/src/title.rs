//! Choosing which of a candidate's titles to score the query against.

use tankovault_domain::compact_key;
use tankovault_domain::matching::Candidate;

use crate::similarity::{edit_ratio, is_token_subset, token_set_ratio};

/// How the query title compares against the best of the candidate's titles.
#[derive(Debug, Clone, Copy)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "the return value of one comparison, private to this module and consumed field by \
              field a few lines later. Packing four independent yes/no answers into flags would \
              hide at the call site exactly what the names make obvious."
)]
pub(crate) struct TitleMatch<'a> {
    /// The best textual agreement in `[0,1]` across all of the candidate's titles.
    pub(crate) ratio: f32,
    /// The best edit-distance ratio specifically, which is what [`MatchSignals::near_identical`]
    /// reports (the overall `ratio` may have come from the token-set view instead).
    pub(crate) edit_ratio: f32,
    pub(crate) exact: bool,
    pub(crate) compact_equal: bool,
    pub(crate) containment: bool,
    /// Whether the winning title was an alternative rather than a primary one.
    pub(crate) via_alias: bool,
    /// Whether the winning title was one of the candidate's source listing titles.
    pub(crate) via_source: bool,
    /// The best textual agreement against the **primary** titles alone (canonical and listing),
    /// whatever won overall. The scorer discounts an alias-only agreement and needs the
    /// undiscounted floor to fall back to; see [`assess_with`](crate::assess::assess_with).
    pub(crate) primary_ratio: f32,
    /// The candidate title the winning comparison was against. Borrowed from the candidate, so
    /// an explanation can name it without the scorer cloning a string on the hot path.
    pub(crate) matched: &'a str,
}

impl Default for TitleMatch<'_> {
    fn default() -> Self {
        Self {
            ratio: 0.0,
            edit_ratio: 0.0,
            exact: false,
            compact_equal: false,
            containment: false,
            via_alias: false,
            via_source: false,
            primary_ratio: 0.0,
            matched: "",
        }
    }
}

/// Which of a candidate's titles a comparison was against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Canonical,
    Source,
    Alias,
}

/// Compare the query against the candidate's canonical title, its sources' listing titles **and**
/// each of its alternatives, keeping the strongest agreement.
///
/// The trigram lookup that produced this candidate already searches `series_titles`, so a
/// candidate can be returned entirely on the strength of a synonym. Scoring only the canonical
/// title then re-judged that candidate against a name it does not go by, which is how a series
/// listed under its romaji title on one provider and its english title on another stayed split
/// even though the retrieval had already connected them.
pub(crate) fn best_title_match<'a>(
    query: &str,
    query_compact: &str,
    candidate: &'a Candidate,
) -> TitleMatch<'a> {
    // The canonical title is the answer to "which title matched?" until something beats it, so
    // an explanation of a pair that agrees on nothing still names a title rather than nothing.
    let mut best = TitleMatch {
        matched: &candidate.normalized_title,
        ..TitleMatch::default()
    };
    // Primary titles first, so a tie goes to the stronger claim.
    let titles = std::iter::once((&candidate.normalized_title, Kind::Canonical))
        .chain(candidate.source_titles.iter().map(|t| (t, Kind::Source)))
        .chain(
            candidate
                .alt_normalized_titles
                .iter()
                .map(|t| (t, Kind::Alias)),
        );

    for (title, kind) in titles {
        let compact = compact_key(title);
        let exact = !query.is_empty() && query == title;
        let compact_equal = !query_compact.is_empty() && query_compact == compact;

        // The token-set view: order- and duplicate-insensitive word overlap.
        let token = token_set_ratio(query, title);
        // The compact view: how many characters apart the two titles are once whitespace stops
        // mattering. Skipped entirely when identity already holds, when either title is too
        // short for the ratio to discriminate, or when the lengths alone rule out a useful
        // score — the last of which is what keeps this off the ingest hot path in the common
        // case of two unrelated titles.
        let edit = if exact || compact_equal {
            1.0
        } else {
            edit_ratio(query_compact, &compact)
        };

        let ratio = if exact || compact_equal {
            1.0
        } else {
            token.max(edit)
        };

        if kind != Kind::Alias {
            best.primary_ratio = best.primary_ratio.max(ratio);
        }
        // `>` not `>=`, so a primary title wins ties against an alternative and `via_alias`
        // reports the truth rather than whichever synonym happened to be last.
        if ratio > best.ratio {
            best.ratio = ratio;
            best.edit_ratio = edit;
            best.exact = exact;
            best.compact_equal = compact_equal;
            best.via_alias = kind == Kind::Alias;
            best.via_source = kind == Kind::Source;
            best.matched = title;
        }
        // Containment is a property of the pair, not of the winning title: an abbreviated
        // canonical title with a fully subtitled synonym should still earn the nudge.
        best.containment |= is_token_subset(query, title) || is_token_subset(title, query);
    }
    best
}
