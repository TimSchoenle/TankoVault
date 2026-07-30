//! Scan triggering (proxied to the control-plane), run status, and the progress stream.

use crate::audit::audit;
use crate::error::{ApiError, ApiResult};
use crate::openapi::ADMIN_SCANS_TAG;
use crate::state::{AppState, AuthUser};
use crate::views::IntoView;
use axum::Json;
use axum::extract::{Path, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::time::Duration;
use tankovault_contracts::admin::ScanTriggeredView;
use tankovault_domain::{Feature, Permission, ProviderId, ScanMode, ScanRun, ScanRunId};
use tokio_stream::StreamExt as _;
use tokio_stream::wrappers::IntervalStream;
use utoipa::ToSchema;

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

    // `scanning.manual` gates this route in the feature table; `scanning.full` gates a *mode*
    // within it, which no route-level rule can express — a full catalogue walk is the
    // expensive one, and an operator throttling a provider needs to stop it without also
    // stopping the cheap latest-feed pass.
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

/// List recent scan runs
///
/// The most recent scan runs (the console's scan-queue overview). The live variant is
/// `/v1/admin/scans/stream`; this GET gives the console its first paint and drives its
/// polling refresh.
#[utoipa::path(
    get,
    path = "/v1/admin/scans",
    tag = ADMIN_SCANS_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Up to 30 most recent scan runs", body = Vec<ScanRun>),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
    )
)]
pub async fn list_scans(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<Vec<ScanRun>>> {
    user.require(Permission::ScansRead).await?;
    Ok(Json(
        tankovault_db::repo::scans::list_recent_runs(&state.pool, 30).await?,
    ))
}

/// List recent scan failures
///
/// The most recently failed scan tasks with their errors, for triaging stuck providers /
/// broken selectors (design §17.2.7).
#[utoipa::path(
    get,
    path = "/v1/admin/scan-failures",
    tag = ADMIN_SCANS_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Up to 25 most recent failed tasks", body = Vec<tankovault_contracts::admin::FailedTaskView>),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
    )
)]
pub async fn scan_failures(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<Vec<tankovault_contracts::admin::FailedTaskView>>> {
    user.require(Permission::ScansRead).await?;
    let rows = tankovault_db::repo::scans::recent_failed_tasks(&state.pool, 25).await?;
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
