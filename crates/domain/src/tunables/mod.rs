//! The recommender's tuning registry — every weight, base and threshold the shelf is built
//! from, its compiled default and the range it may move inside
//! (`docs/RECOMMENDATIONS.md` §8).
//!
//! A deliberate copy of [`crate::Feature`]: the database stores only deviations, so an empty
//! override table is a fully working deployment. Two things differ, and both are in
//! [`Tunable::spec`] — a tunable carries a numeric range rather than a boolean, and it carries
//! [`Applies`], because a value baked into stored model data does not take effect on the next
//! request however loudly the console says it was saved.
//!
//! The registry itself — the [`Tunable`] variants and [`Tunable::spec`] — stays in one file
//! on purpose; see the `expect` on `spec` for why. `taxonomy` holds the classifying enums.

mod taxonomy;

#[cfg(test)]
mod tests;

pub use taxonomy::{Applies, TunableGroup, TunableKind};

use serde::{Deserialize, Serialize};
use std::fmt;
use std::ops::RangeInclusive;
use std::str::FromStr;
use utoipa::ToSchema;

/// One operator-tunable value in the recommendation pipeline.
///
/// Every value is transported and stored as `f64` regardless of what it means; the registry
/// supplies the typing and the readers clamp. A typed column per kind would buy nothing and
/// cost a schema change per knob.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, ToSchema,
)]
#[non_exhaustive]
pub enum Tunable {
    // --- affinity (§4.1) ---
    #[serde(rename = "recsys.affinity.base.completed")]
    AffinityBaseCompleted,
    #[serde(rename = "recsys.affinity.base.reading")]
    AffinityBaseReading,
    #[serde(rename = "recsys.affinity.base.paused")]
    AffinityBasePaused,
    #[serde(rename = "recsys.affinity.base.planned")]
    AffinityBasePlanned,
    #[serde(rename = "recsys.affinity.dropped.floor")]
    AffinityDroppedFloor,
    #[serde(rename = "recsys.affinity.dropped.span")]
    AffinityDroppedSpan,
    #[serde(rename = "recsys.affinity.engagement_knee")]
    AffinityEngagementKnee,
    #[serde(rename = "recsys.affinity.recency_half_life_days")]
    AffinityRecencyHalfLifeDays,
    #[serde(rename = "recsys.affinity.recency_floor")]
    AffinityRecencyFloor,
    #[serde(rename = "recsys.affinity.external_score_weight")]
    AffinityExternalScoreWeight,

    // --- retrieval (§7.1) ---
    #[serde(rename = "recsys.retrieval.seeds")]
    RetrievalSeeds,
    #[serde(rename = "recsys.retrieval.ef_search")]
    RetrievalEfSearch,
    #[serde(rename = "recsys.retrieval.ann_limit_per_seed")]
    RetrievalAnnLimitPerSeed,
    #[serde(rename = "recsys.retrieval.ann_limit_profile")]
    RetrievalAnnLimitProfile,
    #[serde(rename = "recsys.retrieval.exact_feature_limit")]
    RetrievalExactFeatureLimit,
    #[serde(rename = "recsys.retrieval.cooccurrence_seeds")]
    RetrievalCooccurrenceSeeds,
    #[serde(rename = "recsys.retrieval.candidate_cap")]
    RetrievalCandidateCap,

    // --- scoring (§7.2) ---
    #[serde(rename = "recsys.score.weight.knn")]
    ScoreWeightKnn,
    #[serde(rename = "recsys.score.weight.profile")]
    ScoreWeightProfile,
    #[serde(rename = "recsys.score.weight.collaborative")]
    ScoreWeightCollaborative,
    #[serde(rename = "recsys.score.weight.prior")]
    ScoreWeightPrior,
    #[serde(rename = "recsys.score.weight.negative")]
    ScoreWeightNegative,
    #[serde(rename = "recsys.score.cross_type_multiplier")]
    ScoreCrossTypeMultiplier,
    #[serde(rename = "recsys.score.cf_shrinkage_k")]
    ScoreCfShrinkageK,

    // --- diversity (§7.3) ---
    #[serde(rename = "recsys.diversity.lambda")]
    DiversityLambda,
    #[serde(rename = "recsys.diversity.max_per_author")]
    DiversityMaxPerAuthor,
    #[serde(rename = "recsys.diversity.max_per_tag")]
    DiversityMaxPerTag,
    #[serde(rename = "recsys.diversity.max_per_seed")]
    DiversityMaxPerSeed,

    // --- prior (§6.6) ---
    #[serde(rename = "recsys.prior.weight.watchers")]
    PriorWeightWatchers,
    #[serde(rename = "recsys.prior.weight.external_score")]
    PriorWeightExternalScore,
    #[serde(rename = "recsys.prior.weight.source_count")]
    PriorWeightSourceCount,
    #[serde(rename = "recsys.prior.weight.velocity")]
    PriorWeightVelocity,
    #[serde(rename = "recsys.prior.watcher_confidence_k")]
    PriorWatcherConfidenceK,

    // --- build (§6.4) ---
    #[serde(rename = "recsys.build.embedding_dims")]
    BuildEmbeddingDims,
    #[serde(rename = "recsys.build.hnsw_m")]
    BuildHnswM,
    #[serde(rename = "recsys.build.hnsw_ef_construction")]
    BuildHnswEfConstruction,
    #[serde(rename = "recsys.build.min_features")]
    BuildMinFeatures,

    // --- co-occurrence (§6.5) ---
    #[serde(rename = "recsys.cooccurrence.min_support")]
    CooccurrenceMinSupport,
    #[serde(rename = "recsys.cooccurrence.max_list_entries")]
    CooccurrenceMaxListEntries,

    // --- serving (§7.6) ---
    #[serde(rename = "recsys.serve.shelf_size")]
    ServeShelfSize,
    #[serde(rename = "recsys.serve.shelf_ttl_seconds")]
    ServeShelfTtlSeconds,
    #[serde(rename = "recsys.serve.feedback_decay_days")]
    ServeFeedbackDecayDays,

    // --- automatic merges (docs/CONFIGURATION.md §`matching`) ---
    #[serde(rename = "matching.auto_merge")]
    MatchingAutoMerge,
    #[serde(rename = "matching.block_auto_merge_on_numeric_conflict")]
    MatchingBlockOnNumericConflict,
    #[serde(rename = "matching.block_auto_merge_on_author_conflict")]
    MatchingBlockOnAuthorConflict,
    #[serde(rename = "matching.block_auto_merge_on_year_conflict")]
    MatchingBlockOnYearConflict,
    #[serde(rename = "matching.block_auto_merge_on_type_conflict")]
    MatchingBlockOnTypeConflict,
}

/// The compiled facts about one tunable: what it is, what it ships as, and how far it may move.
#[derive(Debug, Clone, Copy)]
pub struct TunableSpec {
    /// The persisted key. Stable forever — it is a database primary key and appears in audit
    /// records.
    pub key: &'static str,
    pub group: TunableGroup,
    pub title: &'static str,
    /// What the value does, written to be read immediately before someone changes production.
    pub description: &'static str,
    pub default: f64,
    /// Inclusive lower bound. Enforced on write *and* clamped on read; see [`Self::range`].
    pub min: f64,
    /// Inclusive upper bound.
    pub max: f64,
    pub kind: TunableKind,
    pub applies: Applies,
}

impl TunableSpec {
    /// The permitted range, inclusive at both ends.
    ///
    /// Stored as two fields rather than a `RangeInclusive` because the registry is built in
    /// `const fn`s and `RangeInclusive::new` is not `const`.
    #[must_use]
    pub const fn range(&self) -> RangeInclusive<f64> {
        self.min..=self.max
    }

    /// `value` confined to the range, with a non-finite input falling back to the default.
    ///
    /// A `NaN` would propagate through every comparison in the ranking silently; it is a stored
    /// value that should not exist, so it is treated like any other out-of-range row.
    #[must_use]
    pub fn clamp(&self, value: f64) -> f64 {
        if value.is_finite() {
            value.clamp(self.min, self.max)
        } else {
            self.default
        }
    }
}

impl Tunable {
    /// Every tunable, in the order the console lists them.
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[
            Self::AffinityBaseCompleted,
            Self::AffinityBaseReading,
            Self::AffinityBasePaused,
            Self::AffinityBasePlanned,
            Self::AffinityDroppedFloor,
            Self::AffinityDroppedSpan,
            Self::AffinityEngagementKnee,
            Self::AffinityRecencyHalfLifeDays,
            Self::AffinityRecencyFloor,
            Self::AffinityExternalScoreWeight,
            Self::RetrievalSeeds,
            Self::RetrievalEfSearch,
            Self::RetrievalAnnLimitPerSeed,
            Self::RetrievalAnnLimitProfile,
            Self::RetrievalExactFeatureLimit,
            Self::RetrievalCooccurrenceSeeds,
            Self::RetrievalCandidateCap,
            Self::ScoreWeightKnn,
            Self::ScoreWeightProfile,
            Self::ScoreWeightCollaborative,
            Self::ScoreWeightPrior,
            Self::ScoreWeightNegative,
            Self::ScoreCrossTypeMultiplier,
            Self::ScoreCfShrinkageK,
            Self::DiversityLambda,
            Self::DiversityMaxPerAuthor,
            Self::DiversityMaxPerTag,
            Self::DiversityMaxPerSeed,
            Self::PriorWeightWatchers,
            Self::PriorWeightExternalScore,
            Self::PriorWeightSourceCount,
            Self::PriorWeightVelocity,
            Self::PriorWatcherConfidenceK,
            Self::BuildEmbeddingDims,
            Self::BuildHnswM,
            Self::BuildHnswEfConstruction,
            Self::BuildMinFeatures,
            Self::CooccurrenceMinSupport,
            Self::CooccurrenceMaxListEntries,
            Self::ServeShelfSize,
            Self::ServeShelfTtlSeconds,
            Self::ServeFeedbackDecayDays,
            Self::MatchingAutoMerge,
            Self::MatchingBlockOnNumericConflict,
            Self::MatchingBlockOnAuthorConflict,
            Self::MatchingBlockOnYearConflict,
            Self::MatchingBlockOnTypeConflict,
        ]
    }

    /// The automatic-merge policy, in the order the console lists it: the threshold, then the
    /// guards that can hold a pair back after it clears the threshold.
    ///
    /// A slice of its own because this group is served by its own endpoint under its own
    /// permission — an operator who may resolve duplicates is not thereby a recommender
    /// tuner — and because the duplicate sweep resolves exactly these five against its
    /// configuration.
    #[must_use]
    pub const fn matching() -> &'static [Self] {
        &[
            Self::MatchingAutoMerge,
            Self::MatchingBlockOnNumericConflict,
            Self::MatchingBlockOnAuthorConflict,
            Self::MatchingBlockOnYearConflict,
            Self::MatchingBlockOnTypeConflict,
        ]
    }

    /// Whether this tunable belongs to the automatic-merge policy rather than the recommender.
    ///
    /// The recommendation endpoints filter on it, so a knob added to one surface cannot appear
    /// on the other by accident.
    #[must_use]
    pub const fn is_matching(self) -> bool {
        matches!(self.spec().group, TunableGroup::Matching)
    }

    /// The five weights that blend the retrieval paths.
    ///
    /// Named as a set because the cross-field rule in §8.3 is about all of them at once: they
    /// need not sum to anything, but all-zero produces an arbitrary shelf and no error anywhere.
    #[must_use]
    pub const fn score_weights() -> &'static [Self] {
        &[
            Self::ScoreWeightKnn,
            Self::ScoreWeightProfile,
            Self::ScoreWeightCollaborative,
            Self::ScoreWeightPrior,
            Self::ScoreWeightNegative,
        ]
    }

    /// The persisted key.
    #[must_use]
    pub const fn key(self) -> &'static str {
        self.spec().key
    }

    /// What this tunable ships as.
    #[must_use]
    pub const fn default_value(self) -> f64 {
        self.spec().default
    }

    /// Whether this tunable's lower bound is a privacy threshold rather than a taste decision.
    ///
    /// The one that is: `recsys.cooccurrence.min_support`. A pair of series may only be
    /// published as "readers of X also read Y" once enough distinct readers stand behind it —
    /// below that, the edge is a statement about identifiable individuals (§12.2). The bound is
    /// therefore refused at the API *and* clamped by every reader, exactly as
    /// [`crate::Feature::is_locked`] is: a stored row that should not exist is a different
    /// failure from a request that should not succeed, and an admin panel that accepts `1` here
    /// is a privacy bug with a user interface.
    #[must_use]
    pub const fn has_privacy_floor(self) -> bool {
        matches!(self, Self::CooccurrenceMinSupport)
    }

    /// The compiled specification.
    #[expect(
        clippy::too_many_lines,
        reason = "one arm per tunable, and the registry is the list: splitting it by group \
                  would put half the ranges somewhere a reader adding a knob never looks"
    )]
    #[must_use]
    pub const fn spec(self) -> TunableSpec {
        use Applies::{Immediately, NextBuild, NextFullBuild, NextSweep};
        use TunableGroup as G;
        use TunableKind::{Count, Days, Ratio, Seconds, Toggle, Weight};

        match self {
            // ----- affinity -----
            Self::AffinityBaseCompleted => TunableSpec {
                key: "recsys.affinity.base.completed",
                group: G::Affinity,
                title: "Completed",
                description: "How much finishing a series says about taste. The strongest \
                              statement the product collects, and the reference every other \
                              status is read against.",
                default: 1.00,
                min: 0.0,
                max: 1.0,
                kind: Ratio,
                applies: Immediately,
            },
            Self::AffinityBaseReading => TunableSpec {
                key: "recsys.affinity.base.reading",
                group: G::Affinity,
                title: "Reading",
                description: "Currently held. Scaled by reading depth, so two chapters in and \
                              two hundred chapters in are not the same signal.",
                default: 0.80,
                min: 0.0,
                max: 1.0,
                kind: Ratio,
                applies: Immediately,
            },
            Self::AffinityBasePaused => TunableSpec {
                key: "recsys.affinity.base.paused",
                group: G::Affinity,
                title: "Paused",
                description: "Ambivalent, not rejected. Also depth-scaled.",
                default: 0.35,
                min: 0.0,
                max: 1.0,
                kind: Ratio,
                applies: Immediately,
            },
            Self::AffinityBasePlanned => TunableSpec {
                key: "recsys.affinity.base.planned",
                group: G::Affinity,
                title: "Plan to read",
                description: "Intent, not taste. A plan-to-read list is aspirational and full \
                              of things nobody opens; raising this above the read statuses \
                              lets a wish list outvote what someone actually read.",
                default: 0.25,
                min: 0.0,
                max: 1.0,
                kind: Ratio,
                applies: Immediately,
            },
            Self::AffinityDroppedFloor => TunableSpec {
                key: "recsys.affinity.dropped.floor",
                group: G::Affinity,
                title: "Dropped early",
                description: "Affinity for a series abandoned at chapter one. Negative by \
                              definition. Pushing it to −1 is the classic mistake: it poisons \
                              the profile of anyone who reads long series.",
                default: -0.60,
                min: -1.0,
                max: 0.0,
                kind: Ratio,
                applies: Immediately,
            },
            Self::AffinityDroppedSpan => TunableSpec {
                key: "recsys.affinity.dropped.span",
                group: G::Affinity,
                title: "Dropped recovery",
                description: "How far a fully committed reader claws back from the dropped \
                              floor. At the defaults, abandoning after 150 chapters lands near \
                              zero rather than near −1 — which is what that actually means.",
                default: 0.50,
                min: 0.0,
                max: 1.0,
                kind: Ratio,
                applies: Immediately,
            },
            Self::AffinityEngagementKnee => TunableSpec {
                key: "recsys.affinity.engagement_knee",
                group: G::Affinity,
                title: "Engagement knee",
                description: "Chapters at which a reader counts as fully committed. Depth is \
                              absolute, not a fraction of the series, so a reader 300 chapters \
                              into a 900-chapter ongoing is not scored as two-thirds invested.",
                default: 60.0,
                min: 5.0,
                max: 1000.0,
                kind: Count,
                applies: Immediately,
            },
            Self::AffinityRecencyHalfLifeDays => TunableSpec {
                key: "recsys.affinity.recency_half_life_days",
                group: G::Affinity,
                title: "Recency half-life",
                description: "Days after which a signal counts half as much. Lower follows \
                              taste drift faster and makes the shelf jumpier.",
                default: 180.0,
                min: 7.0,
                max: 3650.0,
                kind: Days,
                applies: Immediately,
            },
            Self::AffinityRecencyFloor => TunableSpec {
                key: "recsys.affinity.recency_floor",
                group: G::Affinity,
                title: "Recency floor",
                description: "The value recency decay never falls through. At zero a dormant \
                              reader's profile collapses to noise — an all-time favourite is \
                              weaker evidence than last week, never no evidence.",
                default: 0.30,
                min: 0.0,
                max: 1.0,
                kind: Ratio,
                applies: Immediately,
            },
            Self::AffinityExternalScoreWeight => TunableSpec {
                key: "recsys.affinity.external_score_weight",
                group: G::Affinity,
                title: "External score weight",
                description: "How much a linked tracker's own score shifts affinity, centred \
                              on that reader's mean score. Zero ignores external ratings.",
                default: 0.25,
                min: 0.0,
                max: 1.0,
                kind: Weight,
                applies: Immediately,
            },

            // ----- retrieval -----
            Self::RetrievalSeeds => TunableSpec {
                key: "recsys.retrieval.seeds",
                group: G::Retrieval,
                title: "Seeds per request",
                description: "Series used as \"more like this\" anchors. One nearest-neighbour \
                              search each, so this and ef_search are the two knobs that move \
                              p99 latency.",
                // Eight was too few to be about a *reader*: they are the top of the affinity
                // ordering, so a settled shelf explained every pick by the same handful of
                // favourites however many hundred series the reader had actually read. The
                // searches are bounded and cheap; the explanations are what this buys.
                default: 24.0,
                min: 1.0,
                max: 64.0,
                kind: Count,
                applies: Immediately,
            },
            Self::RetrievalEfSearch => TunableSpec {
                key: "recsys.retrieval.ef_search",
                group: G::Retrieval,
                title: "HNSW ef_search",
                description: "Index effort per neighbour search. The one knob that trades ANN \
                              recall against latency with no rebuild — reach for it first in \
                              either direction.",
                default: 60.0,
                min: 10.0,
                max: 1000.0,
                kind: Count,
                applies: Immediately,
            },
            Self::RetrievalAnnLimitPerSeed => TunableSpec {
                key: "recsys.retrieval.ann_limit_per_seed",
                group: G::Retrieval,
                title: "Candidates per seed",
                description: "How many neighbours each seed may contribute before filtering.",
                default: 50.0,
                min: 5.0,
                max: 500.0,
                kind: Count,
                applies: Immediately,
            },
            Self::RetrievalAnnLimitProfile => TunableSpec {
                key: "recsys.retrieval.ann_limit_profile",
                group: G::Retrieval,
                title: "Candidates from the profile vector",
                description: "Neighbours of the reader's centre of gravity. This is the path \
                              that finds what no single seed is near.",
                default: 200.0,
                min: 10.0,
                max: 2000.0,
                kind: Count,
                applies: Immediately,
            },
            Self::RetrievalExactFeatureLimit => TunableSpec {
                key: "recsys.retrieval.exact_feature_limit",
                group: G::Retrieval,
                title: "Exact feature matches",
                description: "Candidates from rare, high-precision features — authors above \
                              all, which the dense space cannot represent at all. Zero \
                              switches that path off.",
                default: 200.0,
                min: 0.0,
                max: 2000.0,
                kind: Count,
                applies: Immediately,
            },
            Self::RetrievalCooccurrenceSeeds => TunableSpec {
                key: "recsys.retrieval.cooccurrence_seeds",
                group: G::Retrieval,
                title: "Co-occurrence seeds",
                description: "Seeds expanded through \"readers of X also read Y\". Zero \
                              switches collaborative retrieval off for every reader.",
                default: 15.0,
                min: 0.0,
                max: 100.0,
                kind: Count,
                applies: Immediately,
            },
            Self::RetrievalCandidateCap => TunableSpec {
                key: "recsys.retrieval.candidate_cap",
                group: G::Retrieval,
                title: "Candidate cap",
                description: "Ceiling on the unioned candidate set before scoring. The \
                              backstop that keeps one request's cost bounded however the paths \
                              above are set.",
                default: 1000.0,
                min: 100.0,
                max: 20000.0,
                kind: Count,
                applies: Immediately,
            },

            // ----- scoring -----
            Self::ScoreWeightKnn => TunableSpec {
                key: "recsys.score.weight.knn",
                group: G::Scoring,
                title: "Weight — nearest neighbours",
                description: "How much a seed's neighbours count. Sub-scores are \
                              rank-normalised per path before blending, so these five weights \
                              need not sum to anything; only their ratios matter.",
                default: 1.00,
                min: 0.0,
                max: 10.0,
                kind: Weight,
                applies: Immediately,
            },
            Self::ScoreWeightProfile => TunableSpec {
                key: "recsys.score.weight.profile",
                group: G::Scoring,
                title: "Weight — taste profile",
                description: "How much the reader's centre of gravity counts against a single \
                              seed's neighbourhood.",
                default: 0.70,
                min: 0.0,
                max: 10.0,
                kind: Weight,
                applies: Immediately,
            },
            Self::ScoreWeightCollaborative => TunableSpec {
                key: "recsys.score.weight.collaborative",
                group: G::Scoring,
                title: "Weight — collaborative",
                description: "How much \"readers of X also read Y\" counts. Worth little on a \
                              deployment with few readers, which is what the model-health \
                              co-occurrence figures are there to tell you.",
                default: 0.60,
                min: 0.0,
                max: 10.0,
                kind: Weight,
                applies: Immediately,
            },
            Self::ScoreWeightPrior => TunableSpec {
                key: "recsys.score.weight.prior",
                group: G::Scoring,
                title: "Weight — popularity prior",
                description: "How much the catalogue's own popularity counts. Raising it makes \
                              every shelf converge on the same well-known series.",
                default: 0.25,
                min: 0.0,
                max: 10.0,
                kind: Weight,
                applies: Immediately,
            },
            Self::ScoreWeightNegative => TunableSpec {
                key: "recsys.score.weight.negative",
                group: G::Scoring,
                title: "Weight — rejection penalty",
                description: "How hard a candidate is penalised for resembling what the reader \
                              has dropped. Zero makes \"I dropped every isekai I opened\" mean \
                              nothing beyond a filter on those four series.",
                default: 0.50,
                min: 0.0,
                max: 10.0,
                kind: Weight,
                applies: Immediately,
            },
            Self::ScoreCrossTypeMultiplier => TunableSpec {
                key: "recsys.score.cross_type_multiplier",
                group: G::Scoring,
                title: "Cross-type multiplier",
                description: "Applied when a candidate's content type differs from the seed's \
                              — manhwa suggested from manga. One treats the types as \
                              interchangeable; zero never crosses.",
                default: 0.70,
                min: 0.0,
                max: 1.0,
                kind: Ratio,
                applies: Immediately,
            },
            Self::ScoreCfShrinkageK => TunableSpec {
                key: "recsys.score.cf_shrinkage_k",
                group: G::Scoring,
                title: "Collaborative shrinkage",
                description: "Pulls a co-occurrence score toward zero until enough readers \
                              stand behind it. Higher distrusts thin evidence more.",
                default: 10.0,
                min: 1.0,
                max: 1000.0,
                kind: Count,
                applies: Immediately,
            },

            // ----- diversity -----
            Self::DiversityLambda => TunableSpec {
                key: "recsys.diversity.lambda",
                group: G::Diversity,
                title: "Relevance vs variety",
                description: "One is pure relevance; lower trades score for distance from what \
                              is already picked. Twelve near-identical series reads as broken \
                              even when every individual pick is defensible.",
                default: 0.70,
                min: 0.0,
                max: 1.0,
                kind: Ratio,
                applies: Immediately,
            },
            Self::DiversityMaxPerAuthor => TunableSpec {
                key: "recsys.diversity.max_per_author",
                group: G::Diversity,
                title: "Max per author",
                description: "Hard cap on one author's share of a shelf. Three books by the \
                              same author is a bibliography, not a recommendation.",
                default: 2.0,
                min: 1.0,
                max: 12.0,
                kind: Count,
                applies: Immediately,
            },
            Self::DiversityMaxPerTag => TunableSpec {
                key: "recsys.diversity.max_per_tag",
                group: G::Diversity,
                title: "Max per tag",
                description: "Hard cap on one genre's share of a shelf.",
                default: 3.0,
                min: 1.0,
                max: 12.0,
                kind: Count,
                applies: Immediately,
            },
            Self::DiversityMaxPerSeed => TunableSpec {
                key: "recsys.diversity.max_per_seed",
                group: G::Diversity,
                title: "Max per seed",
                description: "Hard cap on how many picks one \"because you read\" anchor may \
                              explain. Without it the strongest seed's neighbourhood fills the \
                              shelf, and a reader of hundreds of series is shown one of them.",
                default: 3.0,
                min: 1.0,
                max: 12.0,
                kind: Count,
                applies: Immediately,
            },

            // ----- prior -----
            Self::PriorWeightWatchers => TunableSpec {
                key: "recsys.prior.weight.watchers",
                group: G::Prior,
                title: "Prior — watchers",
                description: "How much local watchlist counts shape the popularity prior. The \
                              only term that depends on this deployment having users.",
                default: 0.40,
                min: 0.0,
                max: 1.0,
                kind: Weight,
                applies: NextBuild,
            },
            Self::PriorWeightExternalScore => TunableSpec {
                key: "recsys.prior.weight.external_score",
                group: G::Prior,
                title: "Prior — external score",
                description: "How much a linked tracker's community score shapes the prior.",
                default: 0.25,
                min: 0.0,
                max: 1.0,
                kind: Weight,
                applies: NextBuild,
            },
            Self::PriorWeightSourceCount => TunableSpec {
                key: "recsys.prior.weight.source_count",
                group: G::Prior,
                title: "Prior — source count",
                description: "How much being carried by many providers shapes the prior. A \
                              proxy for demand that needs no users at all.",
                default: 0.20,
                min: 0.0,
                max: 1.0,
                kind: Weight,
                applies: NextBuild,
            },
            Self::PriorWeightVelocity => TunableSpec {
                key: "recsys.prior.weight.velocity",
                group: G::Prior,
                title: "Prior — release velocity",
                description: "How much recent chapter activity shapes the prior; favours \
                              series that are actually updating.",
                default: 0.15,
                min: 0.0,
                max: 1.0,
                kind: Weight,
                applies: NextBuild,
            },
            Self::PriorWatcherConfidenceK => TunableSpec {
                key: "recsys.prior.watcher_confidence_k",
                group: G::Prior,
                title: "Watcher confidence",
                description: "Watcher count at which the watcher term reaches half its weight. \
                              On a new deployment the raw count is a handful of arbitrary early \
                              watchlists; this is what stops them ranking the catalogue.",
                default: 50.0,
                min: 1.0,
                max: 100_000.0,
                kind: Count,
                applies: NextBuild,
            },

            // ----- build -----
            Self::BuildEmbeddingDims => TunableSpec {
                key: "recsys.build.embedding_dims",
                group: G::Build,
                title: "Embedding dimensions",
                description: "Directions the projection keeps. Baked into every stored vector, \
                              so changing it does nothing until a full rebuild. The upper bound \
                              is the width `series_embedding` is declared with; narrower models \
                              are zero-padded into it, which costs nothing in a cosine.",
                default: 128.0,
                min: 32.0,
                max: 128.0,
                kind: Count,
                applies: NextFullBuild,
            },
            Self::BuildHnswM => TunableSpec {
                key: "recsys.build.hnsw_m",
                group: G::Build,
                title: "HNSW m",
                description: "Graph connectivity. Higher recalls better and costs index memory \
                              roughly linearly. Baked into the built index — needs a rebuild.",
                default: 16.0,
                min: 4.0,
                max: 64.0,
                kind: Count,
                applies: NextFullBuild,
            },
            Self::BuildHnswEfConstruction => TunableSpec {
                key: "recsys.build.hnsw_ef_construction",
                group: G::Build,
                title: "HNSW ef_construction",
                description: "Effort spent building the graph. Higher gives a better graph and \
                              a slower build; it does not affect query cost. Needs a rebuild.",
                default: 64.0,
                min: 16.0,
                max: 512.0,
                kind: Count,
                applies: NextFullBuild,
            },
            Self::BuildMinFeatures => TunableSpec {
                key: "recsys.build.min_features",
                group: G::Build,
                title: "Minimum descriptive features",
                description: "Tags and authors a series needs before it may be recommended at \
                              all. Raising it shrinks coverage — watch the gap between \
                              \"series with features\" and \"recommendable\" on this page.",
                default: 3.0,
                min: 1.0,
                max: 20.0,
                kind: Count,
                applies: NextBuild,
            },

            // ----- co-occurrence -----
            Self::CooccurrenceMinSupport => TunableSpec {
                key: "recsys.cooccurrence.min_support",
                group: G::Cooccurrence,
                title: "Minimum support",
                description: "Distinct readers who must share a pair before it may be \
                              published as \"also read\". This is a privacy threshold, not a \
                              tuning knob: below five, an edge is a statement about \
                              identifiable people. The lower bound cannot be lowered here or \
                              by editing the database.",
                default: 5.0,
                min: 5.0,
                max: 1000.0,
                kind: Count,
                applies: NextBuild,
            },
            Self::CooccurrenceMaxListEntries => TunableSpec {
                key: "recsys.cooccurrence.max_list_entries",
                group: G::Cooccurrence,
                title: "Max stored pairs per series",
                description: "How long each series' \"also read\" list may be. The knob on the \
                              co-occurrence table's size.",
                default: 300.0,
                min: 10.0,
                max: 5000.0,
                kind: Count,
                applies: NextBuild,
            },

            // ----- serving -----
            Self::ServeShelfSize => TunableSpec {
                key: "recsys.serve.shelf_size",
                group: G::Serving,
                title: "Shelf size",
                description: "Recommendations returned when the caller asks for no particular \
                              number, and the ceiling on what it may ask for.",
                // A grid page, not a rail. This used to be twelve, sized for a shelf tucked
                // under the home feed, then twenty-four; recommendations are now a screen of
                // their own, and twenty-four covers is two rows on a wide display — a shelf that
                // reads as having run out on the screen built to browse it.
                default: 60.0,
                min: 1.0,
                max: 120.0,
                kind: Count,
                applies: Immediately,
            },
            Self::ServeShelfTtlSeconds => TunableSpec {
                key: "recsys.serve.shelf_ttl_seconds",
                group: G::Serving,
                title: "Shelf cache lifetime",
                description: "How long a computed shelf may be re-served. A profile rebuild \
                              invalidates it regardless; zero recomputes on every request.",
                default: 21_600.0,
                min: 0.0,
                max: 604_800.0,
                kind: Seconds,
                applies: Immediately,
            },
            Self::ServeFeedbackDecayDays => TunableSpec {
                key: "recsys.serve.feedback_decay_days",
                group: G::Serving,
                title: "Dismissal lifetime",
                description: "How long \"not interested\" suppresses a series. \"Hide forever\" \
                              is unaffected and never expires.",
                default: 90.0,
                min: 1.0,
                max: 3650.0,
                kind: Days,
                applies: Immediately,
            },

            // ----- automatic merges -----
            Self::MatchingAutoMerge => TunableSpec {
                key: "matching.auto_merge",
                group: G::Matching,
                title: "Automatic-merge threshold",
                description: "At or above this score — and only with a structural identity \
                              rule behind it — the duplicate sweep merges two series that \
                              already exist, without asking. Deliberately close to the \
                              ceiling: the merge deletes a row and the id every bookmark and \
                              tracker mapping already names. Lowering it does not loosen the \
                              identity rule, which no score can substitute for.",
                default: 0.97,
                // Not 0.0: an automatic threshold below the review floor would merge everything
                // the sweep shortlists, and "merge on any structural hit" is what 0.6 already
                // means. The bound is what stops a slip of a keystroke collapsing a catalogue.
                min: 0.6,
                max: 1.0,
                kind: Ratio,
                applies: NextSweep,
            },
            Self::MatchingBlockOnNumericConflict => TunableSpec {
                key: "matching.block_auto_merge_on_numeric_conflict",
                group: G::Matching,
                title: "Hold back numbered sequels",
                description: "Titles carrying different numbers (Overlord against Overlord 2) \
                              are reported as distinct rather than queued — the one guard whose \
                              verdict is not review, because queueing a sequel asks an operator \
                              to re-derive the one fact the scorer is certain about. Off makes a \
                              sequel merge-eligible on title similarity alone, and nothing else \
                              in the scorer tells a sequel from its predecessor.",
                default: 1.0,
                min: 0.0,
                max: 1.0,
                kind: Toggle,
                applies: NextSweep,
            },
            Self::MatchingBlockOnAuthorConflict => TunableSpec {
                key: "matching.block_auto_merge_on_author_conflict",
                group: G::Matching,
                title: "Hold back disagreeing credits",
                description: "Both series name authors and share none: a remake, a spin-off, or \
                              an unrelated work with the same title. Costs nothing on a \
                              catalogue whose providers rarely publish credits, because the \
                              signal cannot fire without credits on both sides.",
                default: 1.0,
                min: 0.0,
                max: 1.0,
                kind: Toggle,
                applies: NextSweep,
            },
            Self::MatchingBlockOnYearConflict => TunableSpec {
                key: "matching.block_auto_merge_on_year_conflict",
                group: G::Matching,
                title: "Hold back distant release years",
                description: "Release years three or more years apart. Catches re-serialisations \
                              and remakes sharing an exact title; the scorer's own −0.05 penalty \
                              is smaller than the +0.1 exact-title bonus and so cannot hold such \
                              a pair back on its own.",
                default: 1.0,
                min: 0.0,
                max: 1.0,
                kind: Toggle,
                applies: NextSweep,
            },
            Self::MatchingBlockOnTypeConflict => TunableSpec {
                key: "matching.block_auto_merge_on_type_conflict",
                group: G::Matching,
                title: "Hold back disagreeing media",
                description: "Both series declare a medium and they disagree (manga against \
                              manhwa). Worth switching off on a deployment whose providers infer \
                              the medium from the site they scraped it from rather than from the \
                              work.",
                default: 1.0,
                min: 0.0,
                max: 1.0,
                kind: Toggle,
                applies: NextSweep,
            },
        }
    }

    /// A [`TunableKind::Toggle`]'s value as the switch it represents.
    ///
    /// The midpoint decides, so every stored number means something: a row hand-edited to `0.4`
    /// reads as off rather than as an on-ness no switch can render. Calling this on a tunable
    /// that is not a toggle is a programming error and says so in debug.
    #[must_use]
    pub fn is_on(self, value: f64) -> bool {
        debug_assert_eq!(
            self.spec().kind,
            TunableKind::Toggle,
            "{self} is not a toggle"
        );
        value >= 0.5
    }
}

impl fmt::Display for Tunable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.key())
    }
}

/// Error raised when a stored override names a tunable this build does not have.
#[derive(Debug, Clone, thiserror::Error)]
#[error("unknown tunable: {key:?}")]
pub struct ParseTunableError {
    pub key: String,
}

impl FromStr for Tunable {
    type Err = ParseTunableError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::all()
            .iter()
            .copied()
            .find(|t| t.key() == s)
            .ok_or_else(|| ParseTunableError { key: s.to_owned() })
    }
}
