//! The duplicate-series merge queue: candidates, merges, dismissals, the standing sweep, and
//! the normalized-key rebuild the sweep depends on.

use crate::audit::audit;
use crate::error::{ApiError, ApiResult};
use crate::openapi::ADMIN_MATCHING_TAG;
use crate::state::{AppState, AuthUser};
use crate::views::IntoView;
use axum::Json;
use axum::extract::{Path, Query, State};
use serde::{Deserialize, Serialize};
use tankovault_contracts::admin::{
    KeyRebuildView, MergeCandidateView, MergeFullSweepView, MergePolicyView, MergeSweepView,
};
use tankovault_domain::{Permission, SeriesId, Tunable};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

/// Page size cap for the review queue; safe because results are score-ordered, so a cap only
/// trims the low-confidence end.
const MAX_CANDIDATES: i64 = 200;

/// The control plane's automatic-merge policy route, which both reads and writes go through.
const MERGE_POLICY: &str = "/internal/merge-policy";

#[derive(Debug, Default, Deserialize, IntoParams)]
pub struct CandidateFilter {
    /// Only return candidates at or above this confidence, in `[0,1]`.
    ///
    /// An operator works a large queue in bands: everything above 0.9 is nearly all genuine
    /// duplicates and can be actioned quickly, while the 0.6–0.7 band needs real attention per
    /// row. Out-of-range values are clamped rather than rejected — this narrows a list, so the
    /// worst a nonsense value can do is return nothing.
    #[serde(default)]
    pub min_score: Option<f32>,
}

/// List merge candidates
///
/// The canonicalisation review queue (design §10), highest confidence first.
#[utoipa::path(
    get,
    path = "/v1/admin/merge-candidates",
    tag = ADMIN_MATCHING_TAG,
    params(CandidateFilter),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Up to 200 open merge candidates, highest score first", body = Vec<MergeCandidateView>),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "no second factor is enrolled, or the caller does not hold the required permission", body = crate::error::ProblemDetails),
    )
)]
pub async fn list_merge_candidates(
    State(state): State<AppState>,
    user: AuthUser,
    Query(filter): Query<CandidateFilter>,
) -> ApiResult<Json<Vec<MergeCandidateView>>> {
    user.require(Permission::MergeRead).await?;
    let min_score = filter.min_score.unwrap_or(0.0).clamp(0.0, 1.0);
    let rows = tankovault_db::repo::matching::list_open_merge_candidates(
        &state.pool,
        MAX_CANDIDATES,
        min_score,
    )
    .await?;
    Ok(Json(rows.into_view()))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct MergeRequest {
    /// The surviving canonical series.
    pub keep: SeriesId,
    /// The series merged into `keep` and then deleted.
    pub merge: SeriesId,
}

/// Merge two series
///
/// Transactional re-parent + title/tag/progress/sync union (design §10).
#[utoipa::path(
    post,
    path = "/v1/admin/series/merge",
    tag = ADMIN_MATCHING_TAG,
    request_body = MergeRequest,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Acknowledged", body = serde_json::Value, example = json!({"ok": true})),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "no second factor is enrolled, a step-up is required, or the caller does not hold the required permission", body = crate::error::ProblemDetails),
        (status = 404, description = "One or both series not found", body = crate::error::ProblemDetails),
        (status = 409, description = "`keep` and `merge` are the same series", body = crate::error::ProblemDetails),
    )
)]
pub async fn merge_series(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<MergeRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    user.require(Permission::MergeWrite).await?;
    tankovault_db::repo::matching::merge_series(
        &state.pool,
        req.keep,
        req.merge,
        Some(user.user_id),
        "merged",
    )
    .await?;
    audit(
        &state,
        &user,
        "series.merge",
        &req.merge.to_string(),
        &serde_json::json!({ "keep": req.keep, "merged": req.merge }),
    )
    .await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct DismissRequest {
    pub id: Uuid,
}

/// Dismiss a merge candidate
///
/// Operator judged the two works distinct. The judgement is durable: the pair is suppressed
/// against re-scans and against the standing duplicate sweep, which is what the previous
/// implementation could not promise — a dismissed pair could be re-inserted as a fresh open row
/// by the next scan that saw it.
#[utoipa::path(
    post,
    path = "/v1/admin/merge-candidates/dismiss",
    tag = ADMIN_MATCHING_TAG,
    request_body = DismissRequest,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Whether a candidate was actually dismissed", body = serde_json::Value, example = json!({"dismissed": true})),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "no second factor is enrolled, a step-up is required, or the caller does not hold the required permission", body = crate::error::ProblemDetails),
    )
)]
pub async fn dismiss_merge_candidate(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<DismissRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    user.require(Permission::MergeWrite).await?;
    let dismissed = tankovault_db::repo::matching::dismiss_merge_candidate(
        &state.pool,
        req.id,
        Some(user.user_id),
    )
    .await?;
    audit(
        &state,
        &user,
        "merge_candidate.dismiss",
        &req.id.to_string(),
        &serde_json::json!({ "dismissed": dismissed }),
    )
    .await;
    Ok(Json(serde_json::json!({ "dismissed": dismissed })))
}

/// Run the duplicate sweep
///
/// Re-blocks the whole catalogue on the whitespace-insensitive title key, re-scores every
/// shortlisted pair with everything now known about both series, merges the certain ones and
/// queues the rest.
///
/// Forwarded to the control-plane, which owns the schedule and the leader election that keeps
/// two replicas from racing on the same destructive merge. The scheduled sweep is the normal
/// path; this is here so an operator changing a threshold can see the effect on one run.
#[utoipa::path(
    post,
    path = "/v1/admin/merge-candidates/sweep",
    tag = ADMIN_MATCHING_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "What the sweep did", body = MergeSweepView),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "no second factor is enrolled, a step-up is required, or the caller does not hold the required permission", body = crate::error::ProblemDetails),
        (status = 404, description = "automatic duplicate merging is switched off", body = crate::error::ProblemDetails),
    )
)]
pub async fn sweep_merge_candidates(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<MergeSweepView>> {
    user.require(Permission::MergeWrite).await?;
    // The actor travels with the request so every candidate the sweep resolves records *who*
    // asked for it, rather than being indistinguishable from the hourly schedule.
    let Json(report): Json<MergeSweepView> = state
        .control_plane
        .post(
            "/internal/merge-sweep",
            &serde_json::json!({ "actor": user.user_id }),
        )
        .await?;
    audit(
        &state,
        &user,
        "merge.sweep",
        "-",
        &serde_json::to_value(report).unwrap_or_default(),
    )
    .await;
    Ok(Json(report))
}

/// Run an exhaustive duplicate sweep
///
/// The same sweep as `POST /v1/admin/merge-candidates/sweep`, drawn over and over until a round
/// shortlists nothing it has not already judged — one full rotation of the open queue and the
/// recheck set, and every newly-blocked pair besides. A single sweep only covers one budget's
/// worth of each and leaves the rest to the schedule; this is the button for "look at all of it
/// now", after a normalization change or a policy change that should not wait hours to land.
///
/// It runs to the end: the per-sweep automatic-merge ceiling bounds the *scheduled* sweeps, which
/// merge without anyone watching, and is lifted here because this is an operator asking for the
/// whole catalogue. `scanning.auto_merge` remains the switch for whether it may merge at all.
///
/// The run is detached and takes minutes, so this answers only whether one *started*. Progress,
/// the totals so far and how the last one ended are on `GET` of this same path, which the console
/// polls. A request arriving while a run is live answers `started: false`: the claim is what keeps
/// two runs off the same merges.
#[utoipa::path(
    post,
    path = "/v1/admin/merge-candidates/sweep-all",
    tag = ADMIN_MATCHING_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Whether a run was started", body = MergeFullSweepView),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "no second factor is enrolled, a step-up is required, or the caller does not hold the required permission", body = crate::error::ProblemDetails),
        (status = 404, description = "automatic duplicate merging is switched off", body = crate::error::ProblemDetails),
    )
)]
pub async fn sweep_all_merge_candidates(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<MergeFullSweepView>> {
    user.require(Permission::MergeWrite).await?;
    // The actor travels with the request for the same reason as the single sweep: every merge
    // the run makes is attributable to the person who asked for it, not only to the schedule.
    let Json(view): Json<MergeFullSweepView> = state
        .control_plane
        .post(
            "/internal/merge-sweep-all",
            &serde_json::json!({ "actor": user.user_id }),
        )
        .await?;
    audit(
        &state,
        &user,
        "merge.sweep_all",
        "-",
        &serde_json::to_value(view).unwrap_or_default(),
    )
    .await;
    Ok(Json(view))
}

/// What the exhaustive duplicate sweep is doing, or last did.
///
/// The counters carry the same meanings as [`MergeSweepView`]'s, summed over every round the run
/// has drawn so far. `chains_deferred` has none here: it says the last *pass* skipped a pair
/// because of its own merge, and a run that keeps drawing rounds until nothing new appears has
/// already come back for it.
#[derive(Debug, Serialize, ToSchema)]
pub struct MergeFullSweepStatusView {
    /// Whether a run holds the claim and is still stamping it. A holder whose heartbeat has gone
    /// stale reads `false`, because that is the operator's actual question: is the button live.
    pub running: bool,
    /// RFC 3339. When the current or most recent run started.
    #[schema(example = "2026-08-20T12:00:00Z")]
    pub started_at: Option<String>,
    /// RFC 3339. When the most recent run released its claim; absent while one is running.
    #[schema(example = "2026-08-20T12:09:00Z")]
    pub finished_at: Option<String>,
    /// Rounds drawn. One round is one budgeted sweep.
    pub rounds: i32,
    /// Why the last run stopped: `exhausted` or `failed`. A run has no limit it can stop at, so
    /// anything but a failure means every shortlist was walked out. Absent before the first run,
    /// and while one is in flight; older control planes also wrote `merge_ceiling` and
    /// `round_cap`.
    pub stopped: Option<String>,
    /// How the last run ended when it ended badly. A failed run still releases its claim, so a
    /// failure shows up here rather than as a sweep that never finishes.
    pub error: Option<String>,
    pub pairs_examined: i64,
    pub auto_merged: i64,
    pub queued: i64,
    pub requeued: i64,
    pub reopened: i64,
    pub withdrawn: i64,
    pub distinct: i64,
    pub deferred: i64,
    pub blocked: i64,
}

/// Get the exhaustive duplicate sweep's state
///
/// Progress of the run `POST` of this path starts, and the outcome of the last one. Read from
/// the database rather than from the control plane: the run writes its progress there after
/// every round, and asking the replica that happens to answer would get the state of whichever
/// one was asked rather than of the run.
#[utoipa::path(
    get,
    path = "/v1/admin/merge-candidates/sweep-all",
    tag = ADMIN_MATCHING_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "The exhaustive sweep's current state", body = MergeFullSweepStatusView),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "no second factor is enrolled, or the caller does not hold the required permission", body = crate::error::ProblemDetails),
    )
)]
pub async fn full_merge_sweep_status(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<MergeFullSweepStatusView>> {
    user.require(Permission::MergeRead).await?;
    let sweep = tankovault_db::repo::matching::read_full_sweep_state(&state.pool).await?;
    let rfc3339 = |at: Option<time::OffsetDateTime>| {
        at.and_then(|t| {
            t.format(&time::format_description::well_known::Rfc3339)
                .ok()
        })
    };
    Ok(Json(MergeFullSweepStatusView {
        running: sweep.running,
        started_at: rfc3339(sweep.started_at),
        finished_at: rfc3339(sweep.finished_at),
        rounds: sweep.rounds,
        stopped: sweep.stopped,
        error: sweep.error,
        pairs_examined: sweep.counters.pairs_examined,
        auto_merged: sweep.counters.auto_merged,
        queued: sweep.counters.queued,
        requeued: sweep.counters.requeued,
        reopened: sweep.counters.reopened,
        withdrawn: sweep.counters.withdrawn,
        distinct: sweep.counters.distinct,
        deferred: sweep.counters.deferred,
        blocked: sweep.counters.blocked,
    }))
}

/// Get the automatic-merge policy
///
/// The threshold and the four guards the duplicate sweep applies, each with its effective
/// value, what this deployment falls back to without an override, and who last changed it.
///
/// Read from the control plane rather than resolved here: the fallback is that image's
/// configured `matching` block, and a policy page that showed the *compiled* default would name
/// a value the sweep does not use and reset knobs to it.
#[utoipa::path(
    get,
    path = "/v1/admin/matching/policy",
    tag = ADMIN_MATCHING_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Every knob of the automatic-merge policy", body = Vec<MergePolicyView>),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "no second factor is enrolled, or the caller does not hold the required permission", body = crate::error::ProblemDetails),
    )
)]
pub async fn get_merge_policy(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<Vec<MergePolicyView>>> {
    user.require(Permission::MergeRead).await?;
    let Json(policy) = state.control_plane.get(MERGE_POLICY).await?;
    Ok(Json(policy))
}

/// A proposed new value for one knob of the automatic-merge policy.
#[derive(Debug, Deserialize, ToSchema)]
pub struct SetMergePolicy {
    /// A guard is `1` or `0`; the threshold is a score in its published range.
    pub value: f64,
    /// Why, for the console and the audit record.
    #[serde(default)]
    pub note: Option<String>,
}

/// Set an automatic-merge policy value
///
/// Takes effect on the next sweep, including one an operator starts from this page. Nothing
/// already merged is revisited, and a queued pair keeps its recorded verdict until the sweep
/// re-scores it.
#[utoipa::path(
    put,
    path = "/v1/admin/matching/policy/{key}",
    tag = ADMIN_MATCHING_TAG,
    params(("key" = String, Path, description = "Policy key, e.g. `matching.auto_merge`")),
    request_body = SetMergePolicy,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "The whole policy after the change", body = Vec<MergePolicyView>),
        (status = 400, description = "unknown key, or a value outside its range", body = crate::error::ProblemDetails),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "no second factor is enrolled, a step-up is required, or the caller does not hold the required permission", body = crate::error::ProblemDetails),
    )
)]
pub async fn set_merge_policy(
    State(state): State<AppState>,
    user: AuthUser,
    Path(key): Path<String>,
    Json(body): Json<SetMergePolicy>,
) -> ApiResult<Json<Vec<MergePolicyView>>> {
    user.require(Permission::MergeWrite).await?;
    write_merge_policy(&state, &user, &key, Some(body.value), body.note).await
}

/// Reset an automatic-merge policy value
///
/// Drops the stored override so the knob follows this deployment's configuration again.
/// Distinct from writing that same number, which records a decision that would survive a later
/// change to the configuration.
#[utoipa::path(
    delete,
    path = "/v1/admin/matching/policy/{key}",
    tag = ADMIN_MATCHING_TAG,
    params(("key" = String, Path, description = "Policy key, e.g. `matching.auto_merge`")),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "The whole policy after the reset", body = Vec<MergePolicyView>),
        (status = 400, description = "unknown key", body = crate::error::ProblemDetails),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "no second factor is enrolled, a step-up is required, or the caller does not hold the required permission", body = crate::error::ProblemDetails),
    )
)]
pub async fn reset_merge_policy(
    State(state): State<AppState>,
    user: AuthUser,
    Path(key): Path<String>,
) -> ApiResult<Json<Vec<MergePolicyView>>> {
    user.require(Permission::MergeWrite).await?;
    write_merge_policy(&state, &user, &key, None, None).await
}

/// The half a set and a reset share: refuse an unknown key here, forward the decision, and
/// record who took it.
///
/// The key is parsed against the registry before it travels, so an unknown one is a `400` from
/// this service rather than a round trip that reports the control plane's refusal — and the
/// forwarded key is the registry's own `'static` string, never the caller's.
async fn write_merge_policy(
    state: &AppState,
    user: &AuthUser,
    key: &str,
    value: Option<f64>,
    note: Option<String>,
) -> ApiResult<Json<Vec<MergePolicyView>>> {
    let tunable: Tunable = key
        .parse()
        .ok()
        .filter(|t: &Tunable| t.is_matching())
        .ok_or_else(|| {
            ApiError::BadRequest(format!("no automatic-merge setting is called \"{key}\""))
        })?;

    let Json(policy): Json<Vec<MergePolicyView>> = state
        .control_plane
        .post(
            MERGE_POLICY,
            &serde_json::json!({
                "key": tunable.key(),
                "value": value,
                "note": note,
                "actor": user.user_id,
            }),
        )
        .await?;

    audit(
        state,
        user,
        if value.is_some() {
            "matching.policy.set"
        } else {
            "matching.policy.reset"
        },
        tunable.key(),
        &serde_json::json!({ "value": value, "note": note }),
    )
    .await;
    Ok(Json(policy))
}

/// Rebuild the normalized matching keys
///
/// Re-derives `series.normalized_title` and `series_titles.normalized` for the whole catalogue
/// through the current `normalize_title`.
///
/// # Why this is an operator action
///
/// The normalized title is a *persisted* key: it is written once, when a series is created, and
/// every later match compares against the stored value. A change to the normalization rules —
/// like making an apostrophe join a word instead of splitting one — therefore leaves the entire
/// catalogue on keys derived by the previous rules, and the improvement only reaches rows that
/// happen to be re-scanned. Running this is what makes a rules change take effect, and it is
/// safe to run repeatedly: only rows whose key actually changed are written.
#[utoipa::path(
    post,
    path = "/v1/admin/matching/rebuild-keys",
    tag = ADMIN_MATCHING_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "What the rebuild changed", body = KeyRebuildView),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "no second factor is enrolled, a step-up is required, or the caller does not hold the required permission", body = crate::error::ProblemDetails),
    )
)]
pub async fn rebuild_matching_keys(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<KeyRebuildView>> {
    user.require(Permission::MergeWrite).await?;
    let report = tankovault_db::repo::matching::rebuild_normalized_keys(
        &state.pool,
        tankovault_domain::normalize_title,
    )
    .await?;
    let view = KeyRebuildView {
        series_scanned: report.series_scanned,
        series_updated: report.series_updated,
        titles_scanned: report.titles_scanned,
        titles_updated: report.titles_updated,
        titles_deduplicated: report.titles_deduplicated,
    };
    audit(
        &state,
        &user,
        "matching.rebuild_keys",
        "-",
        &serde_json::to_value(view).unwrap_or_default(),
    )
    .await;
    Ok(Json(view))
}
