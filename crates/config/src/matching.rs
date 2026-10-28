//! The confidence policy that decides whether two titles are the same series.

use serde::Deserialize;
use tankovault_domain::matching::{Candidate, Canonicaliser, Decision, Query};

/// The confidence policy that decides whether two titles are the same series
/// (`docs/ARCHITECTURE.md` §10).
///
/// Ingest and external sync both decide through the [`Canonicaliser`] impl. Defaults are `tankovault_matcher::Thresholds::default()`: 0.85 to attach (an identical title
/// only), 0.6 to flag as a merge candidate.
///
/// ```
/// use tankovault_config::MatchingConfig;
/// use tankovault_domain::matching::{Candidate, Canonicaliser, Decision, Query};
/// use tankovault_domain::{ContentType, SeriesId};
///
/// let existing = SeriesId::new();
/// let query = Query {
///     normalized_title: "solo leveling".to_owned(),
///     content_type: ContentType::Unknown,
///     release_year: None,
///     tags: Vec::new(),
///     authors: Vec::new(),
/// };
/// // An unrelated title scores its trigram similarity alone, so the band edges are exact.
/// let titled = |title: &str, similarity| Candidate {
///     series_id: existing,
///     normalized_title: title.to_owned(),
///     similarity,
///     alt_normalized_titles: Vec::new(),
///     source_titles: Vec::new(),
///     content_type: ContentType::Unknown,
///     release_year: None,
///     tags: Vec::new(),
///     authors: Vec::new(),
/// };
/// let scoring = |similarity| titled("berserk", similarity);
///
/// let policy = MatchingConfig::default();
/// // Only an identical title attaches.
/// assert_eq!(policy.canonicalise(&query, &[titled("solo leveling", 0.2)]), Decision::Attach(existing));
/// assert_eq!(policy.canonicalise(&query, &[scoring(0.3)]), Decision::Create);
///
/// // Similarity alone, however high, creates the series *and* queues the pair for review.
/// for similarity in [0.7, 0.9] {
///     assert!(matches!(
///         policy.canonicalise(&query, &[scoring(similarity)]),
///         Decision::Ambiguous { candidate, .. } if candidate == existing
///     ));
/// }
///
/// // Raising `low` moves the floor: the same candidate is now a new series, not a review row.
/// let cautious = MatchingConfig { low: 0.95, ..MatchingConfig::default() };
/// assert_eq!(cautious.canonicalise(&query, &[scoring(0.9)]), Decision::Create);
///
/// // Boundaries are inclusive on the lower side of each band: an identity scoring exactly
/// // `high` attaches.
/// let exact = MatchingConfig { high: 1.0, ..MatchingConfig::default() };
/// assert_eq!(exact.canonicalise(&query, &[titled("solo leveling", 0.2)]), Decision::Attach(existing));
///
/// // No candidates is the same answer as no good candidate.
/// assert_eq!(policy.canonicalise(&query, &[]), Decision::Create);
/// ```
#[derive(Debug, Clone, Deserialize, terrace_config::schema::Describe)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each bool is one configuration key with its own environment variable, so an \
              operator turns one guard off without restating the other three"
)]
pub struct MatchingConfig {
    /// At or above this score, and only when the incoming title is the series' own title under
    /// an identity rule, the worker attaches to the existing series outright. External sync maps
    /// a remote entry on the score alone.
    #[serde(default = "default_threshold_high")]
    pub high: f32,
    /// At or above this score, a title that did not attach creates the series and flags the pair
    /// for operator review.
    #[serde(default = "default_threshold_low")]
    pub low: f32,
    /// At or above this score, and only when a primary identity rule fired, the duplicate sweep
    /// may merge two existing series without an operator. A corroborated alias is eligible from
    /// [`Self::low`] up, whatever this is set to, because its corroboration, not its score, is the
    /// evidence. Either way an edition conflict, or a numeric conflict with its guard enabled,
    /// reports the pair distinct, and an external-id or live-timing veto or any other enabled
    /// merge guard holds it for review.
    /// [`Self::high`] files an incoming source; this deletes a series that already exists.
    /// `tankovault_matcher::adjudicate` defines the identity rules, vetoes and guards.
    #[serde(default = "default_threshold_auto_merge")]
    pub auto_merge: f32,
    /// How many trigram candidates to score per query title. A higher value widens the index scan.
    #[serde(default = "default_candidate_limit")]
    pub candidate_limit: i64,

    /// Titles carrying different numbers (`Overlord` against `Overlord 2`) are reported as
    /// distinct works. Switching this off makes a sequel merge-eligible on title similarity
    /// alone, and no other rule tells a sequel from its predecessor.
    #[serde(default = "enabled")]
    pub block_auto_merge_on_numeric_conflict: bool,
    /// Both series name authors and share none: a remake, a spin-off, or an unrelated work with
    /// the same title. Off costs nothing on a catalogue whose providers rarely publish credits,
    /// because the signal cannot fire without credits on both sides.
    #[serde(default = "enabled")]
    pub block_auto_merge_on_author_conflict: bool,
    /// Release years three or more years apart, as with re-serialisations and remakes that share
    /// an exact title. The year never moves a score, so this guard is the only way it holds a
    /// pair back.
    #[serde(default = "enabled")]
    pub block_auto_merge_on_year_conflict: bool,
    /// Both series declare a medium and they disagree (manga against manhwa). Worth switching
    /// off on a deployment whose providers guess the medium from the site they scraped it from.
    #[serde(default = "enabled")]
    pub block_auto_merge_on_type_conflict: bool,
}

/// Guards default on. A guard only moves a pair towards operator review, so a wrong one costs a
/// queue row and a missing one costs a deleted series.
const fn enabled() -> bool {
    true
}

fn default_threshold_high() -> f32 {
    tankovault_matcher::Thresholds::default().high
}

fn default_threshold_low() -> f32 {
    tankovault_matcher::Thresholds::default().low
}

fn default_threshold_auto_merge() -> f32 {
    tankovault_matcher::Thresholds::default().auto_merge
}

fn default_candidate_limit() -> i64 {
    10
}

impl Default for MatchingConfig {
    fn default() -> Self {
        Self {
            high: default_threshold_high(),
            low: default_threshold_low(),
            auto_merge: default_threshold_auto_merge(),
            candidate_limit: default_candidate_limit(),
            block_auto_merge_on_numeric_conflict: enabled(),
            block_auto_merge_on_author_conflict: enabled(),
            block_auto_merge_on_year_conflict: enabled(),
            block_auto_merge_on_type_conflict: enabled(),
        }
    }
}

impl MatchingConfig {
    /// Returns the scorer's thresholds and guards, as one value.
    #[must_use]
    pub const fn thresholds(&self) -> tankovault_matcher::Thresholds {
        tankovault_matcher::Thresholds {
            high: self.high,
            low: self.low,
            auto_merge: self.auto_merge,
            guards: tankovault_matcher::MergeGuards {
                numeric_conflict: self.block_auto_merge_on_numeric_conflict,
                author_conflict: self.block_auto_merge_on_author_conflict,
                year_conflict: self.block_auto_merge_on_year_conflict,
                type_conflict: self.block_auto_merge_on_type_conflict,
            },
        }
    }
}

/// The configured policy, as the persistence layer sees it.
///
/// `crates/db` asks for a decision through this impl without depending on `tankovault-matcher`.
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

    /// A candidate whose score is `similarity`, because no other signal is present.
    fn scoring(similarity: f32) -> Candidate {
        titled("berserk", similarity)
    }

    /// A candidate under `title`; with the query's own title it is an identity match.
    fn titled(title: &str, similarity: f32) -> Candidate {
        Candidate {
            series_id: SeriesId::new(),
            normalized_title: title.to_owned(),
            similarity,
            alt_normalized_titles: Vec::new(),
            source_titles: Vec::new(),
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

    /// The configured policy reaches every outcome, `Ambiguous` included. Without it, ingest would
    /// split or merge series with no merge-candidate row to show it.
    #[test]
    fn the_configured_policy_reaches_all_three_outcomes() {
        let cfg = MatchingConfig::default();
        let q = query();

        let attach = titled("solo leveling", 0.2);
        let id = attach.series_id;
        assert_eq!(cfg.canonicalise(&q, &[attach]), Decision::Attach(id));

        let ambiguous = scoring(0.7);
        let ambiguous_id = ambiguous.series_id;
        assert!(matches!(
            cfg.canonicalise(&q, &[ambiguous]),
            Decision::Ambiguous { candidate, .. } if candidate == ambiguous_id
        ));

        assert_eq!(cfg.canonicalise(&q, &[scoring(0.3)]), Decision::Create);
        // No candidates is the same answer as a bad one.
        assert_eq!(cfg.canonicalise(&q, &[]), Decision::Create);
    }

    /// Both bands close at the bottom: an identity scoring exactly `high` attaches, and
    /// `score == low` is ambiguous.
    #[test]
    fn the_bands_are_inclusive_at_their_lower_bound() {
        let cfg = MatchingConfig::default();
        let q = query();

        // An identity scores 1.0, so `high = 1.0` puts it exactly on the boundary.
        let at_high = titled("solo leveling", 0.2);
        let at_high_id = at_high.series_id;
        assert_eq!(
            MatchingConfig {
                high: 1.0,
                ..MatchingConfig::default()
            }
            .canonicalise(&q, &[at_high]),
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

    /// A configured threshold reaches the decision.
    #[test]
    fn tuning_the_thresholds_moves_the_bands() {
        let q = query();
        let candidate = scoring(0.9);
        let strict = MatchingConfig {
            high: 0.95,
            low: 0.94,
            ..MatchingConfig::default()
        };
        assert!(
            matches!(
                MatchingConfig::default().canonicalise(&q, std::slice::from_ref(&candidate)),
                Decision::Ambiguous { .. }
            ),
            "sanity: 0.9 is reviewed under the defaults"
        );
        assert_eq!(
            strict.canonicalise(&q, std::slice::from_ref(&candidate)),
            Decision::Create
        );

        // `high` above every reachable score switches the automatic attach off.
        let identical = titled("solo leveling", 0.2);
        assert!(matches!(
            MatchingConfig {
                high: 1.01,
                ..MatchingConfig::default()
            }
            .canonicalise(&q, &[identical]),
            Decision::Ambiguous { .. }
        ));
    }

    /// `candidate_limit` comes from the policy.
    #[test]
    fn the_candidate_limit_comes_from_the_policy() {
        let cfg = MatchingConfig {
            candidate_limit: 42,
            ..MatchingConfig::default()
        };
        assert_eq!(Canonicaliser::candidate_limit(&cfg), 42);
    }
}
