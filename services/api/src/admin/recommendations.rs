//! The recommender's control plane: model health, the tuning registry, and the rebuild that
//! makes a `NextBuild` change take effect (`docs/RECOMMENDATIONS.md` §8, §10).
//!
//! Modelled on `admin/flags.rs`, down to the response shape: every write answers with the whole
//! registry as the server now resolves it, so the page can never show a value the server does
//! not honour or omit one it does.

use crate::audit::audit;
use crate::error::{ApiError, ApiResult};
use crate::openapi::ADMIN_RECSYS_TAG;
use crate::state::{AppState, AuthUser};
use axum::Json;
use axum::extract::{Path, State};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tankovault_contracts::admin::{RecsysBuildMode, RecsysBuildView};
use tankovault_domain::{Applies, Permission, Tunable, TunableGroup, TunableKind};
use utoipa::ToSchema;

/// One tuning value as the console shows it.
#[derive(Debug, Serialize, ToSchema)]
pub struct TunableView {
    pub key: Tunable,
    pub group: TunableGroup,
    pub title: &'static str,
    /// What the value does, written to be read immediately before someone changes production.
    pub description: &'static str,
    pub kind: TunableKind,
    /// When a change to this value actually reaches a reader. The console shows it on every row:
    /// a knob that needs a rebuild and does not say so is the surface's most likely failure.
    pub applies: Applies,
    /// The effective value: the override if there is one, else the compiled default — already
    /// clamped, so it is exactly what the pipeline reads.
    pub value: f64,
    /// What this value ships as, so the page can show "changed from default" without the client
    /// carrying a copy of the registry.
    pub default_value: f64,
    /// Inclusive bounds. Enforced here, not only in the UI — a `curl` is not an attack.
    pub min: f64,
    pub max: f64,
    /// Whether an operator has explicitly decided this one.
    pub overridden: bool,
    /// Whether [`Self::min`] is a privacy threshold rather than a taste decision. The console
    /// says so next to the field; the API refuses the write regardless.
    pub privacy_floor: bool,
    /// Why it was last changed, if the operator said.
    pub note: Option<String>,
    /// Username of the operator who last changed it; `None` once that account is erased.
    pub updated_by: Option<String>,
    /// When it was last changed. Absent while the value is at its default.
    pub updated_at: Option<String>,
}

/// List recommendation tunables
///
/// Every tuning value this build defines, with its effective value, its shipped default, the
/// range it may move inside, and who last changed it. Served from the compiled registry joined to
/// the stored overrides.
#[utoipa::path(
    get,
    path = "/v1/admin/recommendations/tunables",
    tag = ADMIN_RECSYS_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Every tunable and its current value", body = Vec<TunableView>),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
    )
)]
pub async fn list_tunables(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<Vec<TunableView>>> {
    user.require(Permission::RecsysRead).await?;
    Ok(Json(tunable_views(&state).await?))
}

/// A proposed new value for one tunable.
#[derive(Debug, Deserialize, ToSchema)]
pub struct SetTunable {
    pub value: f64,
    /// Why. Optional, but it is the thing the next operator needs and the audit record does not
    /// surface on this page.
    #[serde(default)]
    pub note: Option<String>,
}

/// Set a recommendation tunable
///
/// Records an explicit decision for one value and applies it immediately on this replica; other
/// replicas pick it up on their next refresh tick. Whether a *reader* sees the difference on the
/// next request depends on the tunable's `applies`.
#[utoipa::path(
    put,
    path = "/v1/admin/recommendations/tunables/{key}",
    tag = ADMIN_RECSYS_TAG,
    params(("key" = String, Path, description = "Tunable key, e.g. `recsys.diversity.lambda`")),
    request_body = SetTunable,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Every tunable and its value after the change", body = Vec<TunableView>),
        (status = 400, description = "unknown tunable, a value outside its range, or a write that would zero every score weight", body = crate::error::ProblemDetails),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
    )
)]
pub async fn set_tunable(
    State(state): State<AppState>,
    user: AuthUser,
    Path(key): Path<String>,
    Json(body): Json<SetTunable>,
) -> ApiResult<Json<Vec<TunableView>>> {
    user.require(Permission::RecsysWrite).await?;
    let tunable = parse_tunable(&key)?;
    let spec = tunable.spec();

    // Refused here as well as clamped by every reader. The clamp is a safety net for a row that
    // should not exist — a restored backup, a rollback, a hand-edited database; this is the door
    // that row should not get through. For `recsys.cooccurrence.min_support` the bound is the
    // k-anonymity threshold of §12.2, so a panel that accepted `1` would be a privacy bug with a
    // user interface.
    if !body.value.is_finite() || !spec.range().contains(&body.value) {
        return Err(ApiError::BadRequest(if tunable.has_privacy_floor() {
            format!(
                "\"{}\" must be between {} and {}. Its lower bound is a privacy threshold, not a \
                 tuning limit: below it, a co-occurrence pair describes identifiable readers.",
                spec.title, spec.min, spec.max
            )
        } else {
            format!(
                "\"{}\" must be between {} and {}, got {}",
                spec.title, spec.min, spec.max, body.value
            )
        }));
    }

    // The one cross-field rule (§8.3). The five weights need not sum to anything — sub-scores are
    // rank-normalised per path before blending, so their scale is free — but all-zero produces an
    // arbitrary shelf and no error anywhere.
    if would_zero_every_score_weight(&state, tunable, body.value).await? {
        return Err(ApiError::BadRequest(
            "at least one score weight must be non-zero: zeroing all five leaves the ranking \
             with nothing to order by, and produces an arbitrary shelf rather than an error"
                .to_owned(),
        ));
    }

    let note = body
        .note
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    tankovault_db::repo::tunables::set_override(
        &state.pool,
        tunable.key(),
        body.value,
        note,
        user.user_id,
    )
    .await?;

    audit(
        &state,
        &user,
        "recsys.tunable.set",
        tunable.key(),
        &serde_json::json!({
            "value": body.value,
            "default": spec.default,
            "applies": spec.applies.as_str(),
            "note": note,
        }),
    )
    .await;

    // Before responding, so the caller's next request already behaves the new way.
    state.tunables.refresh().await;
    tracing::info!(
        tunable = %tunable,
        value = body.value,
        applies = spec.applies.as_str(),
        actor = %user.user_id.as_uuid(),
        "recommendation tunable changed"
    );

    Ok(Json(tunable_views(&state).await?))
}

/// Reset a recommendation tunable
///
/// Drops the stored override so the value follows the compiled default. Distinct from writing
/// that same number, which records an operator decision that would survive a future change of the
/// default.
#[utoipa::path(
    delete,
    path = "/v1/admin/recommendations/tunables/{key}",
    tag = ADMIN_RECSYS_TAG,
    params(("key" = String, Path, description = "Tunable key, e.g. `recsys.diversity.lambda`")),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Every tunable and its value after the reset", body = Vec<TunableView>),
        (status = 400, description = "unknown tunable", body = crate::error::ProblemDetails),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
    )
)]
pub async fn reset_tunable(
    State(state): State<AppState>,
    user: AuthUser,
    Path(key): Path<String>,
) -> ApiResult<Json<Vec<TunableView>>> {
    user.require(Permission::RecsysWrite).await?;
    let tunable = parse_tunable(&key)?;

    let cleared = tankovault_db::repo::tunables::clear_override(&state.pool, tunable.key()).await?;
    audit(
        &state,
        &user,
        "recsys.tunable.reset",
        tunable.key(),
        &serde_json::json!({ "cleared": cleared, "default": tunable.default_value() }),
    )
    .await;

    state.tunables.refresh().await;
    Ok(Json(tunable_views(&state).await?))
}

/// The recommendation model's current state, as an operator needs it before touching anything.
#[derive(Debug, Serialize, ToSchema)]
pub struct ModelHealthView {
    /// The live model generation. A full build takes the next one; an incremental build patches
    /// this one.
    pub generation: i32,
    /// What the builder is doing. `idle` between runs.
    pub stage: String,
    /// Whether a build holds the claim right now.
    pub building: bool,
    /// RFC 3339. When the current or most recent run started.
    #[schema(example = "2026-08-04T12:00:00Z")]
    pub started_at: Option<String>,
    /// RFC 3339. When the most recent run released the claim; absent while one is running.
    #[schema(example = "2026-08-04T12:04:00Z")]
    pub finished_at: Option<String>,
    /// How the last run ended, when it ended badly. The builder always releases its claim, so a
    /// failure shows up here rather than as a build that never finishes.
    pub error: Option<String>,
    /// Series the last run wrote.
    pub series_built: i32,
    /// Distinct features in the vocabulary.
    pub vocabulary: i32,
    /// Width of the dense space the model was built in.
    pub dense_dims: i32,
    pub series_total: i64,
    /// Series with an extracted feature vector.
    pub series_with_features: i64,
    /// Series with a projected embedding, and so reachable by neighbour retrieval.
    pub series_with_embedding: i64,
    /// Series the model is willing to recommend. A large gap below `series_with_embedding` means
    /// `recsys.build.min_features` is excluding more than intended.
    pub series_recommendable: i64,
    /// Series queued for re-embedding, most often after a merge.
    pub repair_queue_depth: i64,
}

/// Get recommendation model health
///
/// Generation, build state, catalogue coverage and repair-queue depth — what an operator needs
/// before changing a tuning value, and the only way to tell an unbuilt model from a broken one.
#[utoipa::path(
    get,
    path = "/v1/admin/recommendations/health",
    tag = ADMIN_RECSYS_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "The model's current state", body = ModelHealthView),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
    )
)]
pub async fn model_health(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<ModelHealthView>> {
    user.require(Permission::RecsysRead).await?;

    let build = tankovault_db::repo::recsys::read_build_state(&state.pool).await?;
    let coverage = tankovault_db::repo::recsys::read_model_coverage(&state.pool).await?;
    let repair_queue_depth = tankovault_db::repo::recsys::repair_depth(&state.pool).await?;

    let rfc3339 = |at: Option<time::OffsetDateTime>| {
        at.and_then(|t| {
            t.format(&time::format_description::well_known::Rfc3339)
                .ok()
        })
    };

    Ok(Json(ModelHealthView {
        generation: build.generation,
        building: build.stage != "idle",
        stage: build.stage,
        started_at: rfc3339(build.started_at),
        finished_at: rfc3339(build.finished_at),
        error: build.error,
        series_built: build.series_built,
        vocabulary: build.vocabulary,
        dense_dims: build.dense_dims,
        series_total: coverage.series_total,
        series_with_features: coverage.with_features,
        series_with_embedding: coverage.with_embedding,
        series_recommendable: coverage.recommendable,
        repair_queue_depth,
    }))
}

/// Which kind of build to run.
#[derive(Debug, Deserialize, ToSchema)]
pub struct RebuildRequest {
    /// `incremental` patches the live generation; `full` re-solves the projection basis and
    /// re-embeds the catalogue, which is what a `next_full_build` tunable needs.
    pub mode: RecsysBuildMode,
}

/// Rebuild the recommendation model
///
/// Runs a build now rather than waiting for the schedule. A `next_build` tuning change takes
/// effect after an incremental run; a `next_full_build` one is baked into stored vectors and the
/// index, and needs `full`.
///
/// The build is a singleton over the whole catalogue, so this runs in the control plane behind
/// the same claim the scheduled runs take.
#[utoipa::path(
    post,
    path = "/v1/admin/recommendations/rebuild",
    tag = ADMIN_RECSYS_TAG,
    request_body = RebuildRequest,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "What the build did", body = RecsysBuildView),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
        (status = 502, description = "the control plane is unreachable", body = crate::error::ProblemDetails),
    )
)]
pub async fn rebuild_model(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<RebuildRequest>,
) -> ApiResult<Json<RecsysBuildView>> {
    user.require(Permission::RecsysWrite).await?;

    let Json(report): Json<RecsysBuildView> = state
        .control_plane
        .post(
            "/internal/recsys-build",
            &serde_json::json!({ "mode": body.mode }),
        )
        .await?;

    let mode = match body.mode {
        RecsysBuildMode::Incremental => "incremental",
        RecsysBuildMode::Full => "full",
    };
    audit(
        &state,
        &user,
        "recsys.rebuild",
        mode,
        &serde_json::to_value(report).unwrap_or_default(),
    )
    .await;
    Ok(Json(report))
}

/// Whether writing `value` to `tunable` would leave every score weight at zero.
///
/// Resolved against the *stored* overrides rather than this replica's snapshot: the snapshot is
/// refreshed on a timer, so a second operator's write from thirty seconds ago might not be in it
/// yet, and this check has to see the state the write actually lands on.
async fn would_zero_every_score_weight(
    state: &AppState,
    tunable: Tunable,
    value: f64,
) -> ApiResult<bool> {
    if !Tunable::score_weights().contains(&tunable) || value != 0.0 {
        return Ok(false);
    }
    let overrides = tankovault_db::repo::tunables::list_overrides(&state.pool).await?;
    let stored: HashMap<&str, f64> = overrides
        .iter()
        .map(|row| (row.key.as_str(), row.value))
        .collect();

    Ok(Tunable::score_weights().iter().all(|&weight| {
        let effective = if weight == tunable {
            value
        } else {
            stored
                .get(weight.key())
                .copied()
                .unwrap_or_else(|| weight.default_value())
        };
        weight.spec().clamp(effective) == 0.0
    }))
}

/// Pairs the compiled registry with stored overrides; iterating the registry (not the table)
/// keeps a removed tunable's stale override from inventing a row nothing reads.
async fn tunable_views(state: &AppState) -> ApiResult<Vec<TunableView>> {
    let overrides = tankovault_db::repo::tunables::list_overrides(&state.pool).await?;
    let by_key: HashMap<&str, &tankovault_db::repo::tunables::TunableOverrideRow> = overrides
        .iter()
        .map(|row| (row.key.as_str(), row))
        .collect();

    Ok(Tunable::all()
        .iter()
        .map(|&tunable| {
            let spec = tunable.spec();
            let stored = by_key.get(spec.key).copied();
            // Clamped, so the page reports what the pipeline reads rather than what the row
            // happens to say — the two differ exactly when a row should not have existed.
            let value = stored.map_or(spec.default, |row| spec.clamp(row.value));
            TunableView {
                key: tunable,
                group: spec.group,
                title: spec.title,
                description: spec.description,
                kind: spec.kind,
                applies: spec.applies,
                value,
                default_value: spec.default,
                min: spec.min,
                max: spec.max,
                overridden: stored.is_some(),
                privacy_floor: tunable.has_privacy_floor(),
                note: stored.and_then(|row| row.note.clone()),
                updated_by: stored.and_then(|row| row.updated_by.clone()),
                updated_at: stored.and_then(|row| {
                    row.updated_at
                        .format(&time::format_description::well_known::Rfc3339)
                        .ok()
                }),
            }
        })
        .collect())
}

fn parse_tunable(key: &str) -> ApiResult<Tunable> {
    key.parse()
        .map_err(|_| ApiError::BadRequest(format!("unknown tunable: {key}")))
}
