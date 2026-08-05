//! Personalised recommendations: the taste profile, the shelf, and the reader's verdict on it.
//!
//! Replaces the stub that scored every series in the catalogue against the reader's tags on every
//! request. Retrieval is now four bounded paths — none of which grows with the catalogue — blended,
//! diversified and explained.

use crate::error::{ApiError, ApiResult};
use crate::openapi::ME_DASHBOARD_TAG;
use crate::state::{AppState, AuthUser};
use axum::Json;
use axum::extract::{Path, Query, State};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use tankovault_db::repo::recsys;
use tankovault_domain::{SeriesId, Tunable, UserId};
use tankovault_recsys::{AffinityParams, Candidate, Path as RetrievalPath, PathWeights};
use tankovault_service::TunableSet;
use utoipa::{IntoParams, ToSchema};

/// How many series shape the taste profile.
///
/// Well above the seed count: the profile vector is a sum over all of them, and truncating it to
/// the seeds would make the reader's *shape* identical to their twenty-five favourites rather
/// than to their taste. Not a tunable — it is a property of what a profile *is*, not a value an
/// operator trades anything against.
const PROFILE_DEPTH: i64 = 200;

/// Features kept in the profile's positive and negative vectors.
const PROFILE_FEATURES: usize = 64;
const NEGATIVE_FEATURES: usize = 32;

/// Over-fetch factor for ANN scans, so post-filtering has candidates left to remove.
const OVERFETCH: i64 = 6;

/// Candidates the prior path may contribute.
///
/// Fixed rather than tunable: the prior is the backfill of last resort and its own weight already
/// controls how much of the shelf it can take.
const PRIOR_CANDIDATES: i64 = 60;

/// Everything the request path reads out of the tuning registry, resolved once per request.
///
/// One read of the snapshot rather than a lock acquisition per candidate, and — more usefully —
/// one place to look for what a request was actually configured with.
struct ShelfTuning {
    affinity: AffinityParams,
    weights: PathWeights,
    seeds: usize,
    per_seed: i64,
    profile_candidates: i64,
    exact_candidates: i64,
    candidate_cap: usize,
    ef_search: i64,
    negative_weight: f32,
    diversity_lambda: f32,
    max_per_author: usize,
    /// Both the default shelf length and the ceiling on what a caller may ask for. One knob,
    /// because the registry publishes one: an operator who wants shorter shelves means shorter
    /// shelves, not a shorter default a query parameter can walk straight past.
    shelf_size: i64,
    shelf_ttl_secs: f64,
    feedback_decay_days: i32,
}

impl ShelfTuning {
    fn read(set: &TunableSet) -> Self {
        Self {
            affinity: AffinityParams {
                base_completed: set.get_f32(Tunable::AffinityBaseCompleted),
                base_reading: set.get_f32(Tunable::AffinityBaseReading),
                base_paused: set.get_f32(Tunable::AffinityBasePaused),
                base_planned: set.get_f32(Tunable::AffinityBasePlanned),
                dropped_floor: set.get_f32(Tunable::AffinityDroppedFloor),
                dropped_span: set.get_f32(Tunable::AffinityDroppedSpan),
                engagement_knee: set.get_f32(Tunable::AffinityEngagementKnee),
                recency_half_life_days: set.get_f32(Tunable::AffinityRecencyHalfLifeDays),
                recency_floor: set.get_f32(Tunable::AffinityRecencyFloor),
            },
            weights: PathWeights {
                seed: set.get_f32(Tunable::ScoreWeightKnn),
                profile: set.get_f32(Tunable::ScoreWeightProfile),
                prior: set.get_f32(Tunable::ScoreWeightPrior),
                exact_premium: tankovault_recsys::ranking::EXACT_PREMIUM,
            },
            seeds: set.get_usize(Tunable::RetrievalSeeds),
            per_seed: set.get_i64(Tunable::RetrievalAnnLimitPerSeed),
            profile_candidates: set.get_i64(Tunable::RetrievalAnnLimitProfile),
            exact_candidates: set.get_i64(Tunable::RetrievalExactFeatureLimit),
            candidate_cap: set.get_usize(Tunable::RetrievalCandidateCap),
            ef_search: set.get_i64(Tunable::RetrievalEfSearch),
            negative_weight: set.get_f32(Tunable::ScoreWeightNegative),
            diversity_lambda: set.get_f32(Tunable::DiversityLambda),
            max_per_author: set.get_usize(Tunable::DiversityMaxPerAuthor),
            shelf_size: set.get_i64(Tunable::ServeShelfSize),
            shelf_ttl_secs: set.get(Tunable::ServeShelfTtlSeconds),
            feedback_decay_days: set.get_i32(Tunable::ServeFeedbackDecayDays),
        }
    }
}

/// One recommended series and why it is here.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Recommendation {
    /// `id`, not `series_id`, on purpose. This endpoint used to return `SeriesSummary`, and the
    /// SPA ships as its own image — so a deployed client always outlives a server change by some
    /// window. Keeping the name makes the new body a strict *superset* of the old one: every
    /// field an older client reads is still there, and the additions are ignored. Renaming it
    /// would have been a deserialization failure in somebody's browser, which is precisely what
    /// the `openapi breaking changes` gate exists to catch.
    pub id: SeriesId,
    pub title: String,
    pub cover_url: Option<String>,
    pub content_type: tankovault_domain::ContentType,
    pub status: tankovault_domain::SeriesStatus,
    pub source_count: i64,
    pub score: f32,
    /// The series that produced this one, when a seed did.
    pub because_series_id: Option<SeriesId>,
    pub because_title: Option<String>,
    /// Features shared with `because_series_id`, most explanatory first.
    pub shared: Vec<String>,
}

/// Query parameters for the shelf.
#[derive(Debug, Deserialize, IntoParams)]
pub struct ShelfParams {
    pub limit: Option<i64>,
}

/// Get "because you read" recommendations
///
/// A personalised shelf: content-similar to what the reader has finished or is deep into,
/// filtered against what they already track or have refused, diversified, and explained.
///
/// Falls back to the catalogue's popularity prior for a reader with no history, and returns an
/// empty array when the recommendation model has never been built.
#[utoipa::path(
    get,
    path = "/v1/me/recommendations",
    tag = ME_DASHBOARD_TAG,
    params(ShelfParams),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "The reader's shelf, best first", body = Vec<Recommendation>),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn recommendations(
    State(state): State<AppState>,
    user: AuthUser,
    Query(params): Query<ShelfParams>,
) -> ApiResult<Json<Vec<Recommendation>>> {
    let started = std::time::Instant::now();
    let tuning = ShelfTuning::read(&state.tunables);
    let limit = params
        .limit
        .unwrap_or(tuning.shelf_size)
        .clamp(1, tuning.shelf_size);
    let profile = ensure_profile(&state, user.user_id, &tuning).await?;

    // Served from cache only when it was built from *this* profile: a rebuild must invalidate the
    // shelf even if the clock has barely moved.
    if let Some(cached) = recsys::read_shelf(
        &state.pool,
        user.user_id,
        profile.built_at,
        tuning.shelf_ttl_secs,
    )
    .await?
        && let Ok(items) = serde_json::from_value::<Vec<Recommendation>>(cached)
    {
        let items: Vec<Recommendation> = items
            .into_iter()
            .take(usize::try_from(limit).unwrap_or(1))
            .collect();
        record_serve(started, "cached", items.len());
        return Ok(Json(items));
    }

    let shelf = compute_shelf(&state, user.user_id, &profile, limit, &tuning).await?;
    if let Ok(items) = serde_json::to_value(&shelf) {
        recsys::write_shelf(&state.pool, user.user_id, &items, profile.built_at).await?;
    }
    record_serve(started, "computed", shelf.len());
    Ok(Json(shelf))
}

/// Record how a shelf was served.
///
/// The size matters as much as the latency: a shelf that is fast because it is empty looks
/// perfectly healthy in a duration histogram, and an unbuilt model produces exactly that.
fn record_serve(started: std::time::Instant, path: &'static str, size: usize) {
    metrics::histogram!(
        tankovault_service::metrics::names::RECSYS_SERVE_DURATION,
        "path" => path
    )
    .record(started.elapsed().as_secs_f64());
    #[expect(clippy::cast_precision_loss, reason = "a shelf is at most sixty items")]
    metrics::histogram!(tankovault_service::metrics::names::RECSYS_SHELF_SIZE).record(size as f64);
}

/// Read the reader's profile, rebuilding it first if it is stale or has never existed.
///
/// Lazily, on the read path, rather than on a background sweep: the profile is one indexed query
/// over the reader's own rows, and a sweep would do that work for every account that never signs
/// in.
async fn ensure_profile(
    state: &AppState,
    user_id: UserId,
    tuning: &ShelfTuning,
) -> ApiResult<recsys::TasteProfile> {
    if let Some(profile) = recsys::read_profile(&state.pool, user_id).await?
        && !profile.stale
    {
        return Ok(profile);
    }
    rebuild_profile(state, user_id, tuning).await?;
    // The row is written by the rebuild, so its absence here would be a lost write rather than an
    // empty profile.
    recsys::read_profile(&state.pool, user_id)
        .await?
        .ok_or(ApiError::Internal)
}

/// Recompute affinity and the taste profile from the reader's watchlist and progress.
async fn rebuild_profile(state: &AppState, user_id: UserId, tuning: &ShelfTuning) -> ApiResult<()> {
    let interactions = recsys::reader_interactions(&state.pool, user_id).await?;

    let mut ids = Vec::with_capacity(interactions.len());
    let mut affinities = Vec::with_capacity(interactions.len());
    let mut engagements = Vec::with_capacity(interactions.len());
    let mut observed = Vec::with_capacity(interactions.len());
    for row in &interactions {
        ids.push(row.series_id);
        affinities.push(tankovault_recsys::affinity(
            row.interaction,
            &tuning.affinity,
        ));
        engagements.push(tankovault_recsys::affinity::engagement(
            row.interaction.chapters_read,
            tuning.affinity.engagement_knee,
        ));
        observed.push(row.observed_at);
    }
    recsys::replace_affinity(
        &state.pool,
        user_id,
        &ids,
        &affinities,
        &engagements,
        &observed,
    )
    .await?;

    let ranked = recsys::top_affinity(&state.pool, user_id, PROFILE_DEPTH).await?;
    let series: Vec<SeriesId> = ranked.iter().map(|(id, _)| *id).collect();
    let (vectors, _) = recsys::weighted_vectors(&state.pool, &series).await?;
    let vector_of: HashMap<SeriesId, Vec<(i32, f32)>> = vectors.into_iter().collect();

    // Two accumulators, not one signed vector. A rejection is evidence about what to avoid, and
    // subtracting it from the positive vector would let a strongly disliked feature cancel a
    // strongly liked one into indifference — which is not what either signal means.
    let mut positive: HashMap<i32, f32> = HashMap::new();
    let mut negative: HashMap<i32, f32> = HashMap::new();
    for (series_id, affinity) in &ranked {
        let Some(vector) = vector_of.get(series_id) else {
            continue;
        };
        let target = if *affinity >= 0.0 {
            &mut positive
        } else {
            &mut negative
        };
        for (feature, weight) in vector {
            *target.entry(*feature).or_insert(0.0) += affinity.abs() * weight;
        }
    }

    let (feature_ids, weights) = top_features(positive, PROFILE_FEATURES);
    let (neg_feature_ids, neg_weights) = top_features(negative, NEGATIVE_FEATURES);

    // Seeds are positives only: "more like this" makes no sense pointed at something rejected.
    let seeds: Vec<SeriesId> = ranked
        .iter()
        .filter(|(_, affinity)| *affinity > 0.0)
        .take(tuning.seeds)
        .map(|(id, _)| *id)
        .collect();
    let embedding = if seeds.is_empty() {
        None
    } else {
        recsys::mean_embedding(&state.pool, &seeds).await?
    };

    recsys::write_profile(
        &state.pool,
        user_id,
        &feature_ids,
        &weights,
        &neg_feature_ids,
        &neg_weights,
        &seeds,
        embedding.as_deref(),
    )
    .await?;
    Ok(())
}

/// Keep the strongest `limit` features, normalised, sorted by id.
///
/// Sorted by *id* because every consumer merges two vectors on that assumption; sorting by weight
/// would silently break `cosine` and `shared_features`.
fn top_features(accumulated: HashMap<i32, f32>, limit: usize) -> (Vec<i32>, Vec<f32>) {
    let mut entries: Vec<(i32, f32)> = accumulated.into_iter().collect();
    entries.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    entries.truncate(limit);
    entries.sort_by_key(|(id, _)| *id);

    let mut weights: Vec<f32> = entries.iter().map(|(_, w)| *w).collect();
    tankovault_recsys::normalise(&mut weights);
    (entries.into_iter().map(|(id, _)| id).collect(), weights)
}

/// Retrieve, blend, filter, diversify and explain.
async fn compute_shelf(
    state: &AppState,
    user_id: UserId,
    profile: &recsys::TasteProfile,
    limit: i64,
    tuning: &ShelfTuning,
) -> ApiResult<Vec<Recommendation>> {
    let suppressed =
        recsys::suppressed_series(&state.pool, user_id, tuning.feedback_decay_days).await?;
    let mut candidates = retrieve(state, profile, &suppressed, tuning).await?;
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    // The backstop on one request's cost, applied after retrieval rather than as a smaller limit
    // on each path: the paths are bounded individually already, and shrinking one of them to fit
    // a global budget would silently make that path's own knob mean something else.
    candidates.truncate(tuning.candidate_cap);
    rank_and_render(state, profile, candidates, limit, tuning).await
}

/// The four retrieval paths, each bounded and none growing with the catalogue.
async fn retrieve(
    state: &AppState,
    profile: &recsys::TasteProfile,
    suppressed: &[SeriesId],
    tuning: &ShelfTuning,
) -> ApiResult<Vec<Candidate<SeriesId>>> {
    let mut candidates: Vec<Candidate<SeriesId>> = Vec::new();

    // One connection for the whole of retrieval, because `hnsw.ef_search` is a session setting.
    // Setting it "on the pool" would configure whichever connection served that one call and
    // leave every search below running on a different one at whatever the last request left
    // behind — a recall knob that appears to work and does nothing.
    let mut conn = state.pool.acquire().await.map_err(|error| {
        tracing::warn!(%error, "no connection available for recommendation retrieval");
        ApiError::Internal
    })?;
    recsys::set_ef_search(&mut conn, tuning.ef_search).await?;

    // R1 — one ANN search per seed. The only path that can say "because you read X".
    for seed in profile.seeds.iter().take(tuning.seeds) {
        let Some(embedding) = recsys::embedding_of(&mut *conn, *seed).await? else {
            continue;
        };
        let found = recsys::nearest_excluding(
            &mut *conn,
            &embedding,
            suppressed,
            false,
            tuning.per_seed,
            tuning.per_seed * OVERFETCH,
        )
        .await?;
        candidates.extend(found.into_iter().map(|n| Candidate {
            id: n.series_id,
            path: RetrievalPath::Seed,
            score: n.score,
            because: Some(*seed),
        }));
    }

    // R2 — one search from the reader's centre of gravity. Catches what no single seed is near.
    if let Some(embedding) = profile.embedding.as_deref() {
        let found = recsys::nearest_excluding(
            &mut *conn,
            embedding,
            suppressed,
            false,
            tuning.profile_candidates,
            tuning.profile_candidates * OVERFETCH,
        )
        .await?;
        candidates.extend(found.into_iter().map(|n| Candidate {
            id: n.series_id,
            path: RetrievalPath::Profile,
            score: n.score,
            because: None,
        }));
    }

    // R3 — exact overlap on the reader's rarest features. This is the path that sees authors,
    // which the embedding cannot represent at all.
    let rare = recsys::rarest_features(&mut *conn, &profile.feature_ids, 8).await?;
    if !rare.is_empty() && tuning.exact_candidates > 0 {
        let found = recsys::exact_feature_matches(
            &mut *conn,
            &rare,
            suppressed,
            false,
            tuning.exact_candidates,
        )
        .await?;
        #[expect(
            clippy::cast_precision_loss,
            reason = "a shared-feature count is a small integer used as a ranking key"
        )]
        candidates.extend(found.into_iter().map(|m| Candidate {
            id: m.series_id,
            path: RetrievalPath::Exact,
            score: m.shared as f32,
            because: None,
        }));
    }

    // R5 — the popularity prior. Cold start, and backfill when the rest come up short.
    let suppressed_set: HashSet<SeriesId> = suppressed.iter().copied().collect();
    let popular = recsys::top_by_prior(&mut *conn, PRIOR_CANDIDATES).await?;
    candidates.extend(
        popular
            .into_iter()
            .filter(|id| !suppressed_set.contains(id))
            .enumerate()
            .map(|(rank, id)| Candidate {
                id,
                path: RetrievalPath::Prior,
                // Descending with rank, so the prior's own ordering survives normalisation.
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "a rank below a few hundred is exact in f32"
                )]
                score: 1.0 / (1.0 + rank as f32),
                because: None,
            }),
    );

    Ok(candidates)
}

/// Blend, penalise against what the reader avoids, diversify, and explain.
async fn rank_and_render(
    state: &AppState,
    profile: &recsys::TasteProfile,
    candidates: Vec<Candidate<SeriesId>>,
    limit: i64,
    tuning: &ShelfTuning,
) -> ApiResult<Vec<Recommendation>> {
    let ranked = tankovault_recsys::blend(&candidates, &tuning.weights);

    // Everything needed to score the negative vector, diversify and explain, in one bounded read.
    let ids: Vec<SeriesId> = ranked.iter().map(|s| s.id).collect();
    let mut wanted = ids.clone();
    wanted.extend(profile.seeds.iter().copied());
    let (vectors, features) = recsys::weighted_vectors(&state.pool, &wanted).await?;
    let vector_of: HashMap<SeriesId, Vec<(i32, f32)>> = vectors.into_iter().collect();
    let feature_of: HashMap<i32, recsys::FeatureRow> =
        features.into_iter().map(|f| (f.id, f)).collect();

    // The negative vector, applied as a penalty. This is what makes "I dropped every isekai I
    // opened" mean something beyond a filter on those four series.
    let negative: Vec<(i32, f32)> = profile
        .neg_feature_ids
        .iter()
        .copied()
        .zip(profile.neg_weights.iter().copied())
        .collect();
    let mut penalised: Vec<tankovault_recsys::Scored<SeriesId>> = ranked
        .into_iter()
        .map(|mut scored| {
            if let Some(vector) = vector_of.get(&scored.id) {
                scored.score -=
                    tuning.negative_weight * tankovault_recsys::cosine(&negative, vector);
            }
            scored
        })
        .collect();
    penalised.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.id.cmp(&b.id)));

    // Diversify over the sparse vectors, then forbid a run by one author outright.
    let empty = Vec::new();
    let similarity = |a: SeriesId, b: SeriesId| {
        tankovault_recsys::cosine(
            vector_of.get(&a).unwrap_or(&empty),
            vector_of.get(&b).unwrap_or(&empty),
        )
    };
    let diversify_limit = usize::try_from(limit)
        .unwrap_or(1)
        .saturating_mul(3)
        .min(penalised.len());
    let diversified = tankovault_recsys::diversify(
        &penalised,
        diversify_limit,
        tuning.diversity_lambda,
        similarity,
    );
    let capped = tankovault_recsys::cap_by(diversified, tuning.max_per_author, |id| {
        vector_of.get(&id).and_then(|vector| {
            vector
                .iter()
                .filter_map(|(feature, _)| feature_of.get(feature))
                .find(|row| row.kind == "author")
                .map(|row| row.value.clone())
        })
    });

    let chosen: Vec<tankovault_recsys::Scored<SeriesId>> = capped
        .into_iter()
        .take(usize::try_from(limit).unwrap_or(1))
        .collect();
    render(state, profile, &chosen, &vector_of, &feature_of).await
}

/// Turn the chosen ranking into the response, with the explanation attached.
///
/// Split from [`rank_and_render`] at the point where the shelf stops being decided and starts
/// being described — which is also where the last database reads happen.
async fn render(
    state: &AppState,
    profile: &recsys::TasteProfile,
    chosen: &[tankovault_recsys::Scored<SeriesId>],
    vector_of: &HashMap<SeriesId, Vec<(i32, f32)>>,
    feature_of: &HashMap<i32, recsys::FeatureRow>,
) -> ApiResult<Vec<Recommendation>> {
    let chosen_ids: Vec<SeriesId> = chosen.iter().map(|s| s.id).collect();
    let summaries = recsys::summaries_in_order(&state.pool, &chosen_ids).await?;
    let title_of: HashMap<SeriesId, String> = summaries
        .iter()
        .map(|item| (item.series.id, item.series.canonical_title.clone()))
        .collect();
    let seed_titles = recsys::summaries_in_order(&state.pool, &profile.seeds).await?;
    let seed_title_of: HashMap<SeriesId, String> = seed_titles
        .into_iter()
        .map(|item| (item.series.id, item.series.canonical_title))
        .collect();
    let score_of: HashMap<SeriesId, (f32, Option<SeriesId>)> = chosen
        .iter()
        .map(|s| (s.id, (s.score, s.because)))
        .collect();

    Ok(summaries
        .into_iter()
        .map(|item| {
            let id = item.series.id;
            let (score, because) = score_of.get(&id).copied().unwrap_or((0.0, None));
            let shared = because
                .and_then(|seed| Some((vector_of.get(&seed)?, vector_of.get(&id)?)))
                .map(|(seed_vector, own)| {
                    tankovault_recsys::shared_features(seed_vector, own, 3)
                        .into_iter()
                        .filter_map(|feature| Some(feature_of.get(&feature)?.value.clone()))
                        .collect()
                })
                .unwrap_or_default();
            Recommendation {
                id,
                title: item.series.canonical_title,
                cover_url: item.series.cover_url,
                content_type: item.series.content_type,
                status: item.series.status,
                source_count: item.source_count,
                score,
                because_series_id: because,
                because_title: because.and_then(|seed| seed_title_of.get(&seed).cloned()),
                shared,
            }
        })
        .filter(|r| title_of.contains_key(&r.id))
        .collect())
}

/// A reader's verdict on a recommendation.
#[derive(Debug, Deserialize, ToSchema)]
pub struct FeedbackBody {
    /// `not_interested` (decays) or `hide_forever` (does not).
    pub verdict: String,
}

/// Dismiss a recommendation
///
/// Records that the reader does not want this series suggested. `not_interested` suppresses it
/// for ninety days; `hide_forever` does not expire. A stronger refusal is never softened by a
/// later weaker one.
#[utoipa::path(
    post,
    path = "/v1/me/recommendations/{series_id}/feedback",
    tag = ME_DASHBOARD_TAG,
    params(("series_id" = SeriesId, Path, description = "Series to suppress")),
    request_body = FeedbackBody,
    security(("bearer_auth" = [])),
    responses(
        (status = 204, description = "Recorded"),
        (status = 400, description = "unknown verdict", body = crate::error::ProblemDetails),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn feedback(
    State(state): State<AppState>,
    user: AuthUser,
    Path(series_id): Path<SeriesId>,
    Json(body): Json<FeedbackBody>,
) -> ApiResult<axum::http::StatusCode> {
    if !matches!(body.verdict.as_str(), "not_interested" | "hide_forever") {
        return Err(ApiError::BadRequest(format!(
            "unknown verdict \"{}\"; expected not_interested or hide_forever",
            body.verdict
        )));
    }
    recsys::record_feedback(&state.pool, user.user_id, series_id, &body.verdict).await?;
    // The shelf was computed without this suppression, so it is wrong the moment this returns.
    recsys::mark_profile_stale(&state.pool, user.user_id).await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

/// The reader's own taste profile, rendered.
#[derive(Debug, Serialize, ToSchema)]
pub struct TasteView {
    /// Features the reader gravitates to, strongest first.
    pub likes: Vec<TasteFeature>,
    /// Features the reader has rejected, strongest first.
    pub avoids: Vec<TasteFeature>,
    /// Series the profile was built from, in affinity order.
    pub seeds: Vec<SeriesId>,
    /// RFC 3339. A `String` rather than an `OffsetDateTime` because `utoipa` has no schema for
    /// the latter, and the published contract should name the format either way.
    #[schema(example = "2026-08-04T12:00:00Z")]
    pub built_at: String,
}

/// One weighted feature in a taste profile.
#[derive(Debug, Serialize, ToSchema)]
pub struct TasteFeature {
    pub kind: String,
    pub value: String,
    pub weight: f32,
}

/// Get the reader's taste profile
///
/// What the recommender believes about this reader, in their own terms. Exists so the profile is
/// inspectable by the person it describes — and so a bad shelf can be diagnosed without anyone
/// reading a watchlist.
#[utoipa::path(
    get,
    path = "/v1/me/taste",
    tag = ME_DASHBOARD_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "The reader's profile", body = TasteView),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn taste(State(state): State<AppState>, user: AuthUser) -> ApiResult<Json<TasteView>> {
    let tuning = ShelfTuning::read(&state.tunables);
    let profile = ensure_profile(&state, user.user_id, &tuning).await?;

    let mut wanted = profile.feature_ids.clone();
    wanted.extend(profile.neg_feature_ids.iter().copied());
    let names = recsys::feature_names(&state.pool, &wanted).await?;
    let name_of: HashMap<i32, (String, String)> = names
        .into_iter()
        .map(|f| (f.id, (f.kind, f.value)))
        .collect();

    let render = |ids: &[i32], weights: &[f32]| -> Vec<TasteFeature> {
        let mut out: Vec<TasteFeature> = ids
            .iter()
            .zip(weights)
            .filter_map(|(id, weight)| {
                let (kind, value) = name_of.get(id)?;
                Some(TasteFeature {
                    kind: kind.clone(),
                    value: value.clone(),
                    weight: *weight,
                })
            })
            .collect();
        out.sort_by(|a, b| b.weight.total_cmp(&a.weight));
        out
    };

    Ok(Json(TasteView {
        likes: render(&profile.feature_ids, &profile.weights),
        avoids: render(&profile.neg_feature_ids, &profile.neg_weights),
        seeds: profile.seeds,
        built_at: profile
            .built_at
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default(),
    }))
}
