//! The recommender's tuning registry — every weight, base and threshold the shelf is built
//! from, its compiled default and the range it may move inside
//! (`docs/RECOMMENDATIONS.md` §8).
//!
//! A deliberate copy of [`crate::Feature`]: the database stores only deviations, so an empty
//! override table is a fully working deployment. Two things differ, and both are in
//! [`Tunable::spec`] — a tunable carries a numeric range rather than a boolean, and it carries
//! [`Applies`], because a value baked into stored model data does not take effect on the next
//! request however loudly the console says it was saved.

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
        ]
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
        use Applies::{Immediately, NextBuild, NextFullBuild};
        use TunableGroup as G;
        use TunableKind::{Count, Days, Ratio, Seconds, Weight};

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
                default: 8.0,
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
                default: 12.0,
                min: 1.0,
                max: 60.0,
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
        }
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

/// The console's grouping of tunables into sections.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum TunableGroup {
    Affinity,
    Retrieval,
    Scoring,
    Diversity,
    Prior,
    Build,
    Cooccurrence,
    Serving,
}

impl TunableGroup {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Affinity => "affinity",
            Self::Retrieval => "retrieval",
            Self::Scoring => "scoring",
            Self::Diversity => "diversity",
            Self::Prior => "prior",
            Self::Build => "build",
            Self::Cooccurrence => "cooccurrence",
            Self::Serving => "serving",
        }
    }
}

impl fmt::Display for TunableGroup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a tunable's number *means*, so the console can render and validate it sensibly.
///
/// Purely presentational: every value is an `f64` on the wire and in the table regardless.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum TunableKind {
    /// A fraction, normally `0…1`.
    Ratio,
    /// A blend coefficient whose scale is only meaningful against the other weights.
    Weight,
    /// A whole number of things; readers round it.
    Count,
    Days,
    Seconds,
}

impl TunableKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ratio => "ratio",
            Self::Weight => "weight",
            Self::Count => "count",
            Self::Days => "days",
            Self::Seconds => "seconds",
        }
    }
}

impl fmt::Display for TunableKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// When a change to a tunable actually reaches a reader.
///
/// The console shows this on every row, because it is the most likely way this surface fails a
/// user: someone raises a value baked into stored model data, sees no change, raises it again,
/// and concludes the page is broken — when the truth is that the old value is what is
/// physically stored.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Applies {
    /// The next request, once each replica's snapshot refreshes.
    Immediately,
    /// The next incremental build.
    NextBuild,
    /// The next full rebuild — the stored model was computed under the old value.
    NextFullBuild,
}

impl Applies {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Immediately => "immediately",
            Self::NextBuild => "next_build",
            Self::NextFullBuild => "next_full_build",
        }
    }
}

impl fmt::Display for Applies {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
#[expect(
    clippy::float_cmp,
    reason = "these compare against exact registry bounds and clamp results, not \n              computed values: a clamp returns its bound bit-for-bit, and a default \n              is the literal in the registry"
)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn every_key_round_trips_and_is_unique() {
        let mut seen = BTreeSet::new();
        for &t in Tunable::all() {
            assert_eq!(Tunable::from_str(t.key()).unwrap(), t);
            assert!(seen.insert(t.key()), "duplicate key {}", t.key());
        }
        assert_eq!(seen.len(), Tunable::all().len());
    }

    #[test]
    fn all_lists_every_variant() {
        // Hand-written and able to drift from the enum; bump it when adding a knob, or a
        // forgotten one is invisible to the console and to every reader.
        assert_eq!(Tunable::all().len(), 41);
    }

    #[test]
    fn serde_uses_the_persisted_key() {
        assert_eq!(
            serde_json::to_string(&Tunable::DiversityLambda).unwrap(),
            "\"recsys.diversity.lambda\""
        );
    }

    /// **The default must be a legal value.**
    ///
    /// The bug this pins: a default outside its own range makes the compiled fallback and the
    /// clamped read disagree, so a deployment with an empty override table runs on a value the
    /// API would refuse to write.
    #[test]
    fn every_default_sits_inside_its_own_range() {
        for &t in Tunable::all() {
            let spec = t.spec();
            assert!(spec.min <= spec.max, "{t} has an inverted range");
            assert!(
                spec.range().contains(&spec.default),
                "{t} defaults to {} outside {:?}",
                spec.default,
                spec.range()
            );
            assert!(spec.clamp(spec.default) - spec.default == 0.0, "{t}");
        }
    }

    #[test]
    fn every_tunable_is_described() {
        for &t in Tunable::all() {
            let spec = t.spec();
            assert!(!spec.title.is_empty(), "{t} has no title");
            assert!(
                spec.description.len() > 30,
                "{t} needs a real description — it is read immediately before someone \
                 changes production"
            );
        }
    }

    /// **The k-anonymity threshold is a floor, not a default.**
    ///
    /// The bug this pins: publishing `min_support` as an ordinary knob with a range starting at
    /// one. Co-occurrence edges below the threshold describe identifiable individuals (§12.2),
    /// so the bound has to be part of the registry rather than a convention the console
    /// remembers.
    #[test]
    fn the_privacy_floor_is_the_bottom_of_its_range_and_survives_clamping() {
        let spec = Tunable::CooccurrenceMinSupport.spec();
        assert!(Tunable::CooccurrenceMinSupport.has_privacy_floor());
        assert_eq!(spec.min, 5.0);
        for attempt in [-100.0, 0.0, 1.0, 4.999] {
            assert_eq!(
                spec.clamp(attempt),
                5.0,
                "a stored {attempt} must still read as 5"
            );
        }
        // Nothing else claims the floor, or the refusal message would name the wrong reason.
        for &t in Tunable::all() {
            assert_eq!(
                t.has_privacy_floor(),
                t == Tunable::CooccurrenceMinSupport,
                "{t} floor"
            );
        }
    }

    /// A non-finite stored value would propagate through every comparison in the ranking
    /// without producing a single error.
    #[test]
    fn a_non_finite_value_falls_back_to_the_default() {
        let spec = Tunable::DiversityLambda.spec();
        assert_eq!(spec.clamp(f64::NAN), spec.default);
        assert_eq!(spec.clamp(f64::INFINITY), spec.default);
        assert_eq!(spec.clamp(f64::NEG_INFINITY), spec.default);
    }

    #[test]
    fn the_score_weights_are_all_in_the_scoring_group() {
        assert_eq!(Tunable::score_weights().len(), 5);
        for &t in Tunable::score_weights() {
            assert_eq!(t.spec().group, TunableGroup::Scoring, "{t}");
            assert_eq!(t.spec().kind, TunableKind::Weight, "{t}");
        }
    }

    /// Every value baked into stored model data must say so, or an operator changes it and
    /// concludes the page is broken when nothing moves (§8.4).
    #[test]
    fn model_shaped_values_declare_that_they_need_a_rebuild() {
        for &t in &[
            Tunable::BuildEmbeddingDims,
            Tunable::BuildHnswM,
            Tunable::BuildHnswEfConstruction,
        ] {
            assert_eq!(t.spec().applies, Applies::NextFullBuild, "{t}");
        }
        for &t in &[
            Tunable::BuildMinFeatures,
            Tunable::CooccurrenceMinSupport,
            Tunable::CooccurrenceMaxListEntries,
            Tunable::PriorWeightWatchers,
        ] {
            assert_eq!(t.spec().applies, Applies::NextBuild, "{t}");
        }
        assert_eq!(
            Tunable::DiversityLambda.spec().applies,
            Applies::Immediately
        );
    }

    #[test]
    fn keys_are_namespaced_under_recsys() {
        for &t in Tunable::all() {
            assert!(t.key().starts_with("recsys."), "{t} is not namespaced");
        }
    }
}
