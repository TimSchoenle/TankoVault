//! Scan triggering (proxied to the control-plane), run status, and the progress stream.

use crate::audit::audit;
use crate::error::{ApiError, ApiResult};
use crate::openapi::ADMIN_SCANS_TAG;
use crate::state::{AppState, AuthUser};
use crate::views::IntoView;
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::time::Duration;
use tankovault_contracts::admin::ScanTriggeredView;
use tankovault_db::repo::scans::ErrorSelector;
use tankovault_domain::{Feature, Permission, ProviderId, RunState, ScanMode, ScanRunId};
use time::OffsetDateTime;
use tokio_stream::StreamExt as _;
use tokio_stream::wrappers::IntervalStream;
use utoipa::{IntoParams, ToSchema};

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct TriggerScan {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<ProviderId>,
    pub mode: ScanMode,
}

/// Trigger a scan
///
/// Proxied to the control-plane planner.
#[utoipa::path(
    post,
    path = "/v1/admin/scans",
    tag = ADMIN_SCANS_TAG,
    request_body = TriggerScan,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Scan queued, forwarded from the control-plane", body = tankovault_contracts::admin::ScanTriggeredView),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "no second factor is enrolled, a step-up is required, or the caller does not hold the required permission", body = crate::error::ProblemDetails),
        (status = 404, description = "manual scanning, or full scans specifically, are switched off", body = crate::error::ProblemDetails),
    )
)]
pub async fn trigger_scan(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<TriggerScan>,
) -> ApiResult<Json<ScanTriggeredView>> {
    user.require(Permission::ScansRun).await?;

    // `scanning.full` gates the mode, not the route: an operator must be able to stop full
    // catalogue walks without also stopping the cheap latest-feed scan.
    if req.mode == ScanMode::Full && !state.features.is_enabled(Feature::ScanningFull) {
        return Err(ApiError::FeatureDisabled(Feature::ScanningFull));
    }

    let Json(body) = state.control_plane.post("/internal/scans", &req).await?;

    audit(
        &state,
        &user,
        "scan.trigger",
        "-",
        &serde_json::to_value(&req).unwrap_or_default(),
    )
    .await;
    Ok(Json(body))
}

/// Get a scan run
#[utoipa::path(
    get,
    path = "/v1/admin/scans/{run_id}",
    tag = ADMIN_SCANS_TAG,
    params(("run_id" = ScanRunId, Path, description = "Scan run id")),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Scan run", body = tankovault_contracts::admin::ScanRunView),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "no second factor is enrolled, or the caller does not hold the required permission", body = crate::error::ProblemDetails),
        (status = 404, description = "Scan run not found", body = crate::error::ProblemDetails),
    )
)]
pub async fn get_scan(
    State(state): State<AppState>,
    user: AuthUser,
    Path(run_id): Path<ScanRunId>,
) -> ApiResult<Json<tankovault_contracts::admin::ScanRunView>> {
    user.require(Permission::ScansRead).await?;
    let run = tankovault_db::repo::scans::get_run(&state.pool, run_id).await?;
    // The slug comes from the run's own page rather than a second lookup: a run linked to from
    // elsewhere may predate the console's window, so the drawer fetches by id, and it needs the
    // same provider label the list shows.
    let provider_slug = match run.provider_id {
        Some(id) => tankovault_db::repo::providers::get(&state.pool, id)
            .await
            .ok()
            .map(|provider| provider.slug),
        None => None,
    };
    Ok(Json(
        tankovault_db::repo::scans::RunListing { run, provider_slug }.into_view(),
    ))
}

/// Run-history paging, capped server-side for the same reason the audit trail is.
const MAX_RUN_PAGE: u32 = 200;
const DEFAULT_RUN_PAGE: u32 = 30;
/// Failure-feed cap. Lower than the run page: a failure carries an error string.
const MAX_FAILURE_PAGE: u32 = 200;
const DEFAULT_FAILURE_PAGE: u32 = 25;

/// How a page of runs is ordered, as the `sort` parameter spells it.
///
/// Its own wire enum rather than the repository's, so an unknown token is a 422 from the
/// extractor instead of a silent fall back to the default ordering — a sort control that
/// quietly ignores what it was asked for is exactly the defect this panel had.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunSortParam {
    /// Newest first.
    #[default]
    Recent,
    Oldest,
    /// Most failed tasks first.
    Failures,
    /// Longest wall-clock first.
    Duration,
}

impl From<RunSortParam> for tankovault_db::repo::scans::RunSort {
    fn from(value: RunSortParam) -> Self {
        match value {
            RunSortParam::Recent => Self::Recent,
            RunSortParam::Oldest => Self::Oldest,
            RunSortParam::Failures => Self::Failures,
            RunSortParam::Duration => Self::Duration,
        }
    }
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct RunQuery {
    /// Provider slug. Absent lists every provider's runs.
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub mode: Option<ScanMode>,
    #[serde(default)]
    pub state: Option<RunState>,
    /// Inclusive lower bound on `created_at`, RFC 3339.
    // `value_type` because `utoipa` has no schema for `OffsetDateTime`; the serde attribute
    // above already pins the wire format this claims.
    #[serde(default, with = "time::serde::rfc3339::option")]
    #[param(value_type = Option<String>)]
    pub since: Option<OffsetDateTime>,
    /// Ordering. Defaults to newest first.
    // Inlined rather than `$ref`d: `IntoParams` does not register the schemas its fields name,
    // so a reference here publishes a component nothing defines and the client generator stops
    // on the dangling pointer.
    #[serde(default)]
    #[param(inline)]
    pub sort: RunSortParam,
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub offset: Option<u32>,
}

/// The window a summary covers.
#[derive(Debug, Deserialize, IntoParams)]
pub struct SummaryQuery {
    /// Provider slug. Absent summarises every provider.
    #[serde(default)]
    pub provider: Option<String>,
    /// Inclusive lower bound, RFC 3339. Absent summarises all of recorded history.
    #[serde(default, with = "time::serde::rfc3339::option")]
    #[param(value_type = Option<String>)]
    pub since: Option<OffsetDateTime>,
}

/// List recent scan runs
///
/// A filtered, paged window on the run history, newest first. The live variant is
/// `/v1/admin/stream`'s `runs` event; this GET is the console's first paint and its
/// manual-refresh path.
#[utoipa::path(
    get,
    path = "/v1/admin/scans",
    tag = ADMIN_SCANS_TAG,
    params(RunQuery),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "A page of scan runs", body = tankovault_contracts::admin::ScanRunPageView),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "no second factor is enrolled, or the caller does not hold the required permission", body = crate::error::ProblemDetails),
    )
)]
pub async fn list_scans(
    State(state): State<AppState>,
    user: AuthUser,
    Query(q): Query<RunQuery>,
) -> ApiResult<Json<tankovault_contracts::admin::ScanRunPageView>> {
    user.require(Permission::ScansRead).await?;
    let limit = q.limit.unwrap_or(DEFAULT_RUN_PAGE).clamp(1, MAX_RUN_PAGE);
    let filter = tankovault_db::repo::scans::RunFilter {
        provider: q
            .provider
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty()),
        mode: q.mode,
        state: q.state,
        since: q.since,
        sort: q.sort.into(),
    };
    let page = tankovault_db::repo::scans::list_runs_filtered(
        &state.pool,
        &filter,
        i64::from(limit),
        i64::from(q.offset.unwrap_or(0)),
    )
    .await?;
    Ok(Json(page.into_view()))
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct FailureQuery {
    /// Provider slug. Absent lists every provider's failures.
    #[serde(default)]
    pub provider: Option<String>,
    /// Inclusive lower bound on `finished_at`, RFC 3339.
    #[serde(default, with = "time::serde::rfc3339::option")]
    #[param(value_type = Option<String>)]
    pub since: Option<OffsetDateTime>,
    /// Include failures an operator has already cleared. Off by default, which is what clearing
    /// is for; cleared failures are acknowledged rather than deleted, so this always reopens the
    /// full window.
    #[serde(default)]
    pub include_cleared: bool,
    #[serde(default)]
    pub limit: Option<u32>,
}

/// List recent scan failures
///
/// The most recently failed scan tasks with their errors, for triaging stuck providers and
/// broken selectors (design §17.2.7). The grouped view is `/v1/admin/scan-failures/grouped`.
#[utoipa::path(
    get,
    path = "/v1/admin/scan-failures",
    tag = ADMIN_SCANS_TAG,
    params(FailureQuery),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Recent failed tasks, newest first", body = Vec<tankovault_contracts::admin::FailedTaskView>),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "no second factor is enrolled, or the caller does not hold the required permission", body = crate::error::ProblemDetails),
    )
)]
pub async fn scan_failures(
    State(state): State<AppState>,
    user: AuthUser,
    Query(q): Query<FailureQuery>,
) -> ApiResult<Json<Vec<tankovault_contracts::admin::FailedTaskView>>> {
    user.require(Permission::ScansRead).await?;
    let limit = q
        .limit
        .unwrap_or(DEFAULT_FAILURE_PAGE)
        .clamp(1, MAX_FAILURE_PAGE);
    let rows = tankovault_db::repo::scans::failed_tasks_filtered(
        &state.pool,
        q.provider
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty()),
        q.since,
        q.include_cleared,
        i64::from(limit),
    )
    .await?;
    Ok(Json(rows.into_view()))
}

/// Group scan failures by error
///
/// The same failures collapsed by their error text, worst first: one broken selector that hit
/// twelve series is one row with a count, not twelve rows of the same sentence.
#[utoipa::path(
    get,
    path = "/v1/admin/scan-failures/grouped",
    tag = ADMIN_SCANS_TAG,
    params(FailureQuery),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Distinct failures with counts and affected providers", body = Vec<tankovault_contracts::admin::FailureGroupView>),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "no second factor is enrolled, or the caller does not hold the required permission", body = crate::error::ProblemDetails),
    )
)]
pub async fn scan_failure_groups(
    State(state): State<AppState>,
    user: AuthUser,
    Query(q): Query<FailureQuery>,
) -> ApiResult<Json<Vec<tankovault_contracts::admin::FailureGroupView>>> {
    user.require(Permission::ScansRead).await?;
    let limit = q
        .limit
        .unwrap_or(DEFAULT_FAILURE_PAGE)
        .clamp(1, MAX_FAILURE_PAGE);
    let rows = tankovault_db::repo::scans::failure_groups(
        &state.pool,
        q.provider
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty()),
        q.since,
        q.include_cleared,
        i64::from(limit),
    )
    .await?;
    Ok(Json(rows.into_view()))
}

/// How many providers the health table carries. Beyond this an operator is reading a directory,
/// not triaging — and the panel orders worst-first, so the tail is the healthy end.
const PROVIDER_HEALTH_LIMIT: i64 = 50;

/// Summarise the scan window
///
/// The same provider and time filter the run list uses, answered as figures: how many runs
/// reached each state, how many tasks succeeded and failed, how many failures are still open,
/// and the per-provider breakdown behind those totals.
#[utoipa::path(
    get,
    path = "/v1/admin/scans/summary",
    tag = ADMIN_SCANS_TAG,
    params(SummaryQuery),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Window rollup with its per-provider breakdown", body = tankovault_contracts::admin::ScanSummaryView),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "no second factor is enrolled, or the caller does not hold the required permission", body = crate::error::ProblemDetails),
    )
)]
pub async fn scan_summary(
    State(state): State<AppState>,
    user: AuthUser,
    Query(q): Query<SummaryQuery>,
) -> ApiResult<Json<tankovault_contracts::admin::ScanSummaryView>> {
    user.require(Permission::ScansRead).await?;
    let provider = q
        .provider
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let summary = tankovault_db::repo::scans::scan_summary(&state.pool, provider, q.since).await?;
    // The breakdown is narrowed by the *window* only: when an operator has already picked one
    // provider, a one-row table tells them nothing they cannot read from the totals, and the
    // comparison against its peers is the reason to look at all.
    let providers = tankovault_db::repo::scans::provider_scan_health(
        &state.pool,
        q.since,
        PROVIDER_HEALTH_LIMIT,
    )
    .await?;
    Ok(Json((summary, providers).into_view()))
}

/// How many settled tasks the activity tail carries.
const ACTIVITY_TAIL: i64 = 15;

/// Live scan activity
///
/// The task-level state of the runs in flight: what each is holding, since when, and what
/// settled most recently. The live variant is `/v1/admin/stream`'s `activity` event; this GET is
/// the panel's first paint, because a live tail that starts empty for three seconds reads as an
/// idle deployment.
#[utoipa::path(
    get,
    path = "/v1/admin/scans/activity",
    tag = ADMIN_SCANS_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "In-flight runs and the tail of settled tasks", body = tankovault_contracts::admin::ScanActivityView),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "no second factor is enrolled, or the caller does not hold the required permission", body = crate::error::ProblemDetails),
    )
)]
pub async fn scan_activity(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<tankovault_contracts::admin::ScanActivityView>> {
    user.require(Permission::ScansRead).await?;
    let runs = tankovault_db::repo::scans::active_run_activity(&state.pool).await?;
    let events =
        tankovault_db::repo::scans::recent_task_activity(&state.pool, ACTIVITY_TAIL).await?;
    Ok(Json(tankovault_contracts::admin::ScanActivityView {
        runs: runs.into_view(),
        events: events.into_view(),
    }))
}

/// Which error group a clear request names, from the two fields the body carries.
///
/// The pairing that matters is an absent `error` with `match_null_error` unset: that is "any
/// error", not "the group with no error". Reading it the other way would turn a plain "clear
/// everything shown" into a request that clears almost nothing — or, inverted, turn "clear the
/// failures with no message" into clearing the entire feed.
const fn selected_error(error: Option<&str>, match_null: bool) -> ErrorSelector<'_> {
    match (error, match_null) {
        (Some(text), _) => ErrorSelector::Exactly(text),
        (None, true) => ErrorSelector::Absent,
        (None, false) => ErrorSelector::Any,
    }
}

/// Clear scan failures
///
/// Acknowledges the selected failures so they leave the triage feed. Nothing is deleted: the
/// task keeps its `failed` state, its error and its contribution to the run counters, so the
/// history still reconciles and `include_cleared=true` reopens the full window.
///
/// Behind `scans.run` rather than `scans.read`, because it changes what every *other* operator
/// sees: a reader entitled to watch the queue must not be able to hide an outage from the person
/// on call.
#[utoipa::path(
    post,
    path = "/v1/admin/scan-failures/clear",
    tag = ADMIN_SCANS_TAG,
    request_body = tankovault_contracts::admin::ClearFailuresBody,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "How many failures this call cleared", body = tankovault_contracts::admin::FailuresClearedView),
        (status = 400, description = "`since` is not an RFC 3339 instant", body = crate::error::ProblemDetails),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "no second factor is enrolled, a step-up is required, or the caller does not hold the required permission", body = crate::error::ProblemDetails),
    )
)]
pub async fn clear_scan_failures(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<tankovault_contracts::admin::ClearFailuresBody>,
) -> ApiResult<Json<tankovault_contracts::admin::FailuresClearedView>> {
    user.require(Permission::ScansRun).await?;

    let since = body
        .since
        .as_deref()
        .map(|raw| OffsetDateTime::parse(raw, &time::format_description::well_known::Rfc3339))
        .transpose()
        .map_err(|_| ApiError::BadRequest("`since` must be an RFC 3339 instant".into()))?;
    let selector = tankovault_db::repo::scans::FailureSelector {
        provider: body
            .provider
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty()),
        since,
        run_id: body.run_id,
        error: selected_error(body.error.as_deref(), body.match_null_error),
    };
    let cleared = tankovault_db::repo::scans::clear_failures(&state.pool, &selector).await?;

    audit(
        &state,
        &user,
        "scan.failures.clear",
        body.run_id
            .map(|id| id.to_string())
            .as_deref()
            .unwrap_or("-"),
        &serde_json::json!({
            "provider": body.provider,
            "since": body.since,
            "error": body.error,
            "match_null_error": body.match_null_error,
            "cleared": cleared,
        }),
    )
    .await;

    Ok(Json(tankovault_contracts::admin::FailuresClearedView {
        cleared: i64::try_from(cleared).unwrap_or(i64::MAX),
    }))
}

/// Live scan-progress stream
///
/// SSE live scan progress for the operator console. Polls the durable `scan_runs` (the system
/// of record for progress) every 2 s and pushes a `runs` event; a `scan.progress` NATS relay
/// is a documented enhancement.
#[utoipa::path(
    get,
    path = "/v1/admin/scans/stream",
    tag = ADMIN_SCANS_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "SSE stream of `runs` events", content_type = "text/event-stream"),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "no second factor is enrolled, or the caller does not hold the required permission", body = crate::error::ProblemDetails),
    )
)]
pub async fn scan_stream(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>>> {
    user.require(Permission::ScansRead).await?;
    let pool = state.pool.clone();
    let stream =
        IntervalStream::new(tokio::time::interval(Duration::from_secs(2))).then(move |_| {
            let pool = pool.clone();
            async move {
                let runs = tankovault_db::repo::scans::list_recent_runs(&pool, 20)
                    .await
                    .unwrap_or_default()
                    .into_view();
                let event = Event::default()
                    .event("runs")
                    .json_data(&runs)
                    .unwrap_or_else(|_| Event::default().comment("serialize error"));
                Ok(event)
            }
        });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

#[cfg(test)]
mod tests {
    use super::{ErrorSelector, RunSortParam, selected_error};
    use tankovault_db::repo::scans::RunSort;

    /// The three-way selection a clear request carries, and the one collapse that would be a
    /// data-loss bug: an absent `error` is "any error", so reading it as the null group would
    /// make a request to clear the no-message failures clear every failure in the window.
    #[test]
    fn an_absent_error_selects_every_group_and_never_the_null_one() {
        assert_eq!(selected_error(None, false), ErrorSelector::Any);
        assert_eq!(selected_error(None, true), ErrorSelector::Absent);
        assert_eq!(
            selected_error(Some("http 503"), false),
            ErrorSelector::Exactly("http 503")
        );
        // An explicit error wins: the flag only decides what an *absent* one means, so a client
        // sending both must not have its named group widened to the null one.
        assert_eq!(
            selected_error(Some("http 503"), true),
            ErrorSelector::Exactly("http 503")
        );
    }

    /// The wire token and the repository's ordering must not drift: the parameter is published
    /// in `openapi.json` and the frontend picks from it, so a mismapped arm is a sort control
    /// that silently orders by something else — the defect class this panel already had once.
    #[test]
    fn every_sort_token_maps_to_its_own_ordering() {
        let pairs = [
            (RunSortParam::Recent, RunSort::Recent, "recent"),
            (RunSortParam::Oldest, RunSort::Oldest, "oldest"),
            (RunSortParam::Failures, RunSort::Failures, "failures"),
            (RunSortParam::Duration, RunSort::Duration, "duration"),
        ];
        for (param, expected, token) in pairs {
            let mapped: RunSort = param.into();
            assert_eq!(mapped, expected, "`{token}` maps to the wrong ordering");
            assert_eq!(mapped.token(), token);
        }
    }

    /// An omitted `sort` must be the newest-first ordering the panel and the API document both
    /// claim as the default.
    #[test]
    fn the_default_ordering_is_newest_first() {
        let mapped: RunSort = RunSortParam::default().into();
        assert_eq!(mapped, RunSort::Recent);
    }
}
