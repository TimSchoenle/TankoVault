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
use tankovault_domain::{Feature, Permission, ProviderId, RunState, ScanMode, ScanRun, ScanRunId};
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
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
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
        (status = 200, description = "Scan run", body = ScanRun),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
        (status = 404, description = "Scan run not found", body = crate::error::ProblemDetails),
    )
)]
pub async fn get_scan(
    State(state): State<AppState>,
    user: AuthUser,
    Path(run_id): Path<ScanRunId>,
) -> ApiResult<Json<ScanRun>> {
    user.require(Permission::ScansRead).await?;
    Ok(Json(
        tankovault_db::repo::scans::get_run(&state.pool, run_id).await?,
    ))
}

/// Run-history paging, capped server-side for the same reason the audit trail is.
const MAX_RUN_PAGE: u32 = 200;
const DEFAULT_RUN_PAGE: u32 = 30;
/// Failure-feed cap. Lower than the run page: a failure carries an error string.
const MAX_FAILURE_PAGE: u32 = 200;
const DEFAULT_FAILURE_PAGE: u32 = 25;

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
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub offset: Option<u32>,
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
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
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
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
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
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
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
        i64::from(limit),
    )
    .await?;
    Ok(Json(rows.into_view()))
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
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
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
                    .unwrap_or_default();
                let event = Event::default()
                    .event("runs")
                    .json_data(&runs)
                    .unwrap_or_else(|_| Event::default().comment("serialize error"));
                Ok(event)
            }
        });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}
