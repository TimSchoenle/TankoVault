//! The confidence policy for deciding "is this the same series?".

use serde::Deserialize;
use tankovault_domain::matching::{Candidate, Canonicaliser, Decision, Query};

/// The confidence policy for deciding "is this the same series?" (design §10).
///
/// # Why this is shared configuration
///
/// Two code paths make that decision — the worker's ingest canonicalisation
/// (`tankovault_db::repo::catalog::resolve_canonical_series`) and external sync's remote-entry
/// resolution (`services/sync`'s `SeriesResolver`) — using the same scorer. They used to take
/// their thresholds from different places, one of them hardcoded inside the persistence layer, so
/// the worker could attach a source that sync would refuse to map with no single place to reason
/// about it (ARCH-16). Tuning matching is one decision; it is made here.
///
/// This type is therefore not only a bag of knobs: it *is* the policy object, via its
/// [`Canonicaliser`] impl. `crates/db` names that port and nothing else, so the persistence
/// layer performs a decision it does not make and links no scorer.
///
/// The defaults are the scorer's own (`tankovault_matcher::Thresholds::default()`): 0.85 to
/// attach, 0.6 to flag as a merge candidate. Raising `high` makes the matcher more conservative
/// (more duplicate series, fewer wrong merges); lowering it does the reverse.
#[derive(Debug, Clone, Deserialize)]
pub struct MatchingConfig {
    /// At or above this score, attach to the existing series outright.
    #[serde(default = "default_threshold_high")]
    pub high: f32,
    /// At or above this score but below [`Self::high`], create the series but flag the pair for
    /// operator review.
    #[serde(default = "default_threshold_low")]
    pub low: f32,
    /// How many trigram candidates to score per query title. More costs a wider index scan and
    /// buys nothing once the true match is in the set.
    #[serde(default = "default_candidate_limit")]
    pub candidate_limit: i64,
}

fn default_threshold_high() -> f32 {
    tankovault_matcher::Thresholds::default().high
}

fn default_threshold_low() -> f32 {
    tankovault_matcher::Thresholds::default().low
}

fn default_candidate_limit() -> i64 {
    10
}

impl Default for MatchingConfig {
    fn default() -> Self {
        Self {
            high: default_threshold_high(),
            low: default_threshold_low(),
            candidate_limit: default_candidate_limit(),
        }
    }
}

impl MatchingConfig {
    /// The scorer's threshold pair.
    #[must_use]
    pub const fn thresholds(&self) -> tankovault_matcher::Thresholds {
        tankovault_matcher::Thresholds {
            high: self.high,
            low: self.low,
        }
    }
}

/// The configured policy, as the persistence layer sees it (ARCH-16 step 3).
///
/// This impl is the *only* place the configured thresholds meet the scorer. It is what lets
/// `crates/db` read trigram candidates and perform an outcome without depending on
/// `tankovault-matcher` at all: the repository asks, this decides.
impl Canonicaliser for MatchingConfig {
    fn candidate_limit(&self) -> i64 {
        self.candidate_limit
    }

    fn canonicalise(&self, query: &Query, candidates: &[Candidate]) -> Decision {
        tankovault_matcher::decide(query, candidates, self.thresholds())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tankovault_domain::{ContentType, SeriesId};

    /// A candidate whose score *is* `similarity`: unknown medium on both sides, no year, no
    /// tags or authors, and a title sharing no word with the query, so every bonus and penalty
    /// in `tankovault_matcher::score` is switched off and the band boundaries are exact.
    fn scoring(similarity: f32) -> Candidate {
        Candidate {
            series_id: SeriesId::new(),
            normalized_title: "berserk".to_owned(),
            similarity,
            content_type: ContentType::Unknown,
            release_year: None,
            tags: Vec::new(),
            authors: Vec::new(),
        }
    }

    fn query() -> Query {
        Query {
            normalized_title: "solo leveling".to_owned(),
            content_type: ContentType::Unknown,
            release_year: None,
            tags: Vec::new(),
            authors: Vec::new(),
        }
    }

    /// The three outcomes the persistence layer performs, reached through the *configured*
    /// policy rather than through `matcher::decide` directly.
    ///
    /// `crates/db` no longer knows that thresholds exist (ARCH-16 step 3); it calls
    /// [`Canonicaliser::canonicalise`] and writes what comes back. If this impl ever stopped
    /// returning `Ambiguous` for the middle band, ingest would silently either split one work
    /// across two series or merge two different ones, with no merge-candidate row to notice it.
    #[test]
    fn the_configured_policy_reaches_all_three_outcomes() {
        let cfg = MatchingConfig::default();
        let q = query();

        let attach = scoring(0.9);
        let id = attach.series_id;
        assert_eq!(cfg.canonicalise(&q, &[attach]), Decision::Attach(id));

        let ambiguous = scoring(0.7);
        let ambiguous_id = ambiguous.series_id;
        assert!(matches!(
            cfg.canonicalise(&q, &[ambiguous]),
            Decision::Ambiguous { candidate, .. } if candidate == ambiguous_id
        ));

        assert_eq!(cfg.canonicalise(&q, &[scoring(0.3)]), Decision::Create);
        // No candidates at all is the same answer as a bad one: the first source of a work has
        // nothing to match against.
        assert_eq!(cfg.canonicalise(&q, &[]), Decision::Create);
    }

    /// Both bands are closed at the bottom: `score == high` attaches and `score == low` is
    /// ambiguous, not the other way round.
    ///
    /// An off-by-one-band comparison here is invisible in integration tests — every realistic
    /// score is nowhere near a boundary — and would quietly move thousands of series between
    /// "auto-attached" and "queued for an operator".
    #[test]
    fn the_bands_are_inclusive_at_their_lower_bound() {
        let cfg = MatchingConfig::default();
        let q = query();

        let at_high = scoring(cfg.high);
        let at_high_id = at_high.series_id;
        assert_eq!(
            cfg.canonicalise(&q, &[at_high]),
            Decision::Attach(at_high_id),
            "score == high must attach"
        );

        let at_low = scoring(cfg.low);
        assert!(
            matches!(cfg.canonicalise(&q, &[at_low]), Decision::Ambiguous { .. }),
            "score == low must be ambiguous, not Create"
        );

        // Just below `low` is the other side of that boundary.
        assert_eq!(
            cfg.canonicalise(&q, &[scoring(cfg.low - 0.01)]),
            Decision::Create
        );
    }

    /// A configured threshold actually reaches the decision.
    ///
    /// This is the ARCH-16 defect in miniature: the worker's canonicalisation used to score
    /// against `Thresholds::default()` hardcoded inside `crates/db`, so operator-tuned values
    /// changed what `services/sync` would map and nothing about what the worker attached. A
    /// score that is `Attach` under the defaults must become `Create` under a strict policy.
    #[test]
    fn tuning_the_thresholds_moves_the_bands() {
        let q = query();
        let candidate = scoring(0.9);
        let strict = MatchingConfig {
            high: 0.95,
            low: 0.94,
            ..MatchingConfig::default()
        };
        assert_eq!(
            MatchingConfig::default().canonicalise(&q, std::slice::from_ref(&candidate)),
            Decision::Attach(candidate.series_id),
            "sanity: 0.9 attaches under the defaults"
        );
        assert_eq!(
            strict.canonicalise(&q, std::slice::from_ref(&candidate)),
            Decision::Create
        );
    }

    /// `candidate_limit` travels with the policy rather than with the query, so widening the
    /// search is one configuration change and not a per-call-site argument.
    #[test]
    fn the_candidate_limit_comes_from_the_policy() {
        let cfg = MatchingConfig {
            candidate_limit: 42,
            ..MatchingConfig::default()
        };
        assert_eq!(Canonicaliser::candidate_limit(&cfg), 42);
    }
}
