//! The confidence policy for deciding "is this the same series?".

use serde::Deserialize;
use tankovault_domain::matching::{Candidate, Canonicaliser, Decision, Query};

/// The confidence policy for deciding "is this the same series?" (design §10).
///
/// Ingest canonicalisation and external sync both decide through this one scorer via the
/// [`Canonicaliser`] impl below, so `crates/db` names the port without depending on the scorer.
/// Defaults are `tankovault_matcher::Thresholds::default()`: 0.85 to attach, 0.6 to flag as a
/// merge candidate.
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
/// // No corroborating signal, so the score is just the trigram similarity — band edges exact.
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
/// let policy = MatchingConfig::default();
/// assert_eq!(policy.canonicalise(&query, &[scoring(0.9)]), Decision::Attach(existing));
/// assert_eq!(policy.canonicalise(&query, &[scoring(0.3)]), Decision::Create);
///
/// // The middle band creates the series *and* queues the pair for operator review.
/// assert!(matches!(
///     policy.canonicalise(&query, &[scoring(0.7)]),
///     Decision::Ambiguous { candidate, .. } if candidate == existing
/// ));
///
/// // Raising `high` moves the band: the same candidate now needs review instead of attaching.
/// let cautious = MatchingConfig { high: 0.95, ..MatchingConfig::default() };
/// assert!(matches!(
///     cautious.canonicalise(&query, &[scoring(0.9)]),
///     Decision::Ambiguous { .. }
/// ));
///
/// // Boundaries are inclusive on the lower side of each band: `score == high` attaches.
/// let exact = MatchingConfig { high: 0.9, low: 0.6, ..MatchingConfig::default() };
/// assert_eq!(exact.canonicalise(&query, &[scoring(0.9)]), Decision::Attach(existing));
///
/// // No candidates is the same answer as no good candidate.
/// assert_eq!(policy.canonicalise(&query, &[]), Decision::Create);
/// ```
#[derive(Debug, Clone, Deserialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each bool is one independently-settable configuration key, named in \
              docs/CONFIGURATION.md and set by its own environment variable. The lint's \
              suggested remedy — a state machine or two-variant enums — would stop an operator \
              turning one guard off without restating the other three."
)]
pub struct MatchingConfig {
    /// At or above this score, attach to the existing series outright.
    #[serde(default = "default_threshold_high")]
    pub high: f32,
    /// At or above this score but below [`Self::high`], create the series but flag the pair for
    /// operator review.
    #[serde(default = "default_threshold_low")]
    pub low: f32,
    /// At or above this score — **and** only when a structural identity rule fired — the
    /// duplicate sweep merges two existing series without an operator. A separate knob from
    /// [`Self::high`]: that files an incoming source, this deletes one that already exists.
    /// See `tankovault_matcher::adjudicate` for the structural half of the bar.
    #[serde(default = "default_threshold_auto_merge")]
    pub auto_merge: f32,
    /// How many trigram candidates to score per query title; more only costs a wider index scan.
    #[serde(default = "default_candidate_limit")]
    pub candidate_limit: i64,

    /// Titles carrying different numbers (`Overlord` against `Overlord 2`) are reported as
    /// distinct works rather than queued. Switching this off makes a sequel merge-eligible on
    /// title similarity alone, which is the single most expensive mistake this scorer can make —
    /// there is no other rule that distinguishes a sequel from its predecessor.
    #[serde(default = "enabled")]
    pub block_auto_merge_on_numeric_conflict: bool,
    /// Both series name authors and share none: a remake, a spin-off, or an unrelated work with
    /// the same title. Off costs nothing on a catalogue whose providers rarely publish credits,
    /// because the signal cannot fire without credits on both sides.
    #[serde(default = "enabled")]
    pub block_auto_merge_on_author_conflict: bool,
    /// Release years three or more years apart. Catches re-serialisations and remakes that share
    /// an exact title; the scorer's own -0.05 penalty is smaller than the exact-title bonus and
    /// so cannot hold such a pair back on its own.
    #[serde(default = "enabled")]
    pub block_auto_merge_on_year_conflict: bool,
    /// Both series declare a medium and they disagree (manga against manhwa). Worth switching
    /// off on a deployment whose providers guess the medium from the site they scraped it from.
    #[serde(default = "enabled")]
    pub block_auto_merge_on_type_conflict: bool,
}

/// Guards default on: a guard only ever moves a pair *towards* operator review, so the cost of
/// a wrong one is a queue row and the cost of a missing one is a deleted series.
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
    /// The scorer's thresholds and guards, as one value.
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
/// The only place the configured thresholds meet the scorer, letting `crates/db` ask for a
/// decision without depending on `tankovault-matcher`.
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

    /// A candidate whose score *is* `similarity`: no other signal is present, so every
    /// bonus/penalty in `score` is switched off and band boundaries are exact.
    fn scoring(similarity: f32) -> Candidate {
        Candidate {
            series_id: SeriesId::new(),
            normalized_title: "berserk".to_owned(),
            similarity,
            alt_normalized_titles: Vec::new(),
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

    /// Reached through the *configured* policy, not `matcher::decide` directly. If this ever
    /// stopped returning `Ambiguous` for the middle band, ingest would silently split or merge
    /// series with no merge-candidate row to notice it.
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
        // No candidates is the same answer as a bad one.
        assert_eq!(cfg.canonicalise(&q, &[]), Decision::Create);
    }

    /// Both bands close at the bottom: `score == high` attaches, `score == low` is ambiguous.
    /// An off-by-one here is invisible in integration tests and would silently move series
    /// between auto-attach and operator review.
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

    /// A configured threshold actually reaches the decision: canonicalisation once scored
    /// against hardcoded defaults, so operator tuning changed nothing about what attached.
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

    /// `candidate_limit` travels with the policy, not as a per-call-site argument.
    #[test]
    fn the_candidate_limit_comes_from_the_policy() {
        let cfg = MatchingConfig {
            candidate_limit: 42,
            ..MatchingConfig::default()
        };
        assert_eq!(Canonicaliser::candidate_limit(&cfg), 42);
    }
}
