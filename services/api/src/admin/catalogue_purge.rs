//! The catalogue purge: a detached run that empties the catalogue in batches, and the routes that
//! start it, report on it and cancel it.
//!
//! The purge used to run inside the request, deleting batches until a deadline and answering
//! with what was left, with the console calling it in a loop. A single batch on a large
//! catalogue could outlast the 30 s request timeout on its own; the timeout then dropped the
//! handler mid-transaction, the batch rolled back, and the purge could never make progress.
//! Nothing in a request's lifetime bounds this run now. It commits one batch at a time, sizes
//! each batch to a time target, and records its progress in `catalogue_purge_state`, which the
//! console polls.

use super::catalogue::DeletionView;
use crate::audit::{audit, audit_failure};
use crate::error::{ApiError, ApiResult};
use crate::openapi::ADMIN_CATALOGUE_TAG;
use crate::state::{AppState, AuthUser};
use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tankovault_db::PgPool;
use tankovault_db::repo::catalog::maintenance::{self, DeletionReport};
use tankovault_db::repo::catalog::purge::{
    self as purge_state, PurgeClaim, PurgeState, PurgeStop, PurgeTarget,
};
use tankovault_domain::Permission;
use tankovault_service::{AuditEvent, AuditOutcome};
use time::OffsetDateTime;
use utoipa::ToSchema;

/// How long one batch should take. Short enough that the row locks a batch holds never stall a
/// running scan for long, long enough that per-batch overhead stays negligible.
const BATCH_TARGET: Duration = Duration::from_secs(2);

/// Consecutive failed batches a run absorbs before it stops. The purge races live scans for the
/// same rows, so a deadlock or serialization failure is expected now and then, and a rolled-back
/// batch loses nothing.
const MAX_BATCH_FAILURES: u32 = 3;

/// The pause before retrying a failed batch.
const RETRY_PAUSE: Duration = Duration::from_secs(1);

/// How much of the catalogue a purge takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PurgeScope {
    /// Every chapter. Series and their provider sources survive, so the next scan refills them.
    Chapters,
    /// Every series, and with it every source, chapter, watchlist entry and reading position
    /// that hung off one.
    Everything,
}

impl PurgeScope {
    /// The word the caller has to echo back. Its own token, so a request that names one scope
    /// and confirms another is refused rather than resolved in either direction.
    const fn token(self) -> &'static str {
        self.target().as_str()
    }

    const fn target(self) -> PurgeTarget {
        match self {
            Self::Chapters => PurgeTarget::Chapters,
            Self::Everything => PurgeTarget::Everything,
        }
    }
}

impl From<PurgeTarget> for PurgeScope {
    fn from(target: PurgeTarget) -> Self {
        match target {
            PurgeTarget::Chapters => Self::Chapters,
            PurgeTarget::Everything => Self::Everything,
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct PurgeRequest {
    pub scope: PurgeScope,
    /// The scope's own token, echoed back. Same guard as `confirm_username` on the account
    /// erasure paths: it is what stops a mis-aimed script from emptying a deployment on a
    /// request body it built by accident.
    pub confirm: String,
}

/// Why a purge run stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = CataloguePurgeStop)]
pub enum PurgeStopView {
    /// Nothing of the purged kind is left.
    Done,
    /// An operator cancelled it.
    Cancelled,
    /// A batch removed nothing while rows remained.
    Stalled,
    /// A batch failed; `error` says why.
    Failed,
    /// The process running it stopped without finishing. Starting the purge again resumes it.
    Interrupted,
}

impl From<PurgeStop> for PurgeStopView {
    fn from(stop: PurgeStop) -> Self {
        match stop {
            PurgeStop::Done => Self::Done,
            PurgeStop::Cancelled => Self::Cancelled,
            PurgeStop::Stalled => Self::Stalled,
            PurgeStop::Failed => Self::Failed,
            PurgeStop::Interrupted => Self::Interrupted,
        }
    }
}

/// The purge's current or most recent run.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = CataloguePurgeStatus)]
pub struct PurgeStatusView {
    /// Whether a run holds the claim and is still making progress.
    pub running: bool,
    /// The scope of the current or last run; absent before the first.
    pub scope: Option<PurgeScope>,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub started_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub finished_at: Option<OffsetDateTime>,
    /// Whether a cancel has been asked of the running purge. It stops after its current batch.
    pub cancel_requested: bool,
    /// What the run has removed so far.
    pub removed: DeletionView,
    /// Rows of the purged kind left after the last committed batch; absent until one commits.
    pub remaining: Option<i64>,
    /// Why the last run stopped; absent while one is running and before the first.
    pub stopped: Option<PurgeStopView>,
    /// Why the last run failed.
    pub error: Option<String>,
}

impl From<PurgeState> for PurgeStatusView {
    fn from(state: PurgeState) -> Self {
        Self {
            running: state.running,
            scope: state.scope.map(PurgeScope::from),
            started_at: state.started_at,
            finished_at: state.finished_at,
            cancel_requested: state.cancel_requested,
            removed: state.removed.into(),
            remaining: state.remaining,
            stopped: state.stopped.map(PurgeStopView::from),
            error: state.error,
        }
    }
}

/// Whether a purge was started, and the state it is in.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = CataloguePurgeStart)]
pub struct PurgeStartView {
    /// `false` when a run was already live; `status` is then that run's.
    pub started: bool,
    pub status: PurgeStatusView,
}

/// Start a catalogue purge
///
/// Starts a detached run that empties the catalogue in batches, and answers only whether it
/// started. Progress is on `GET` of this path. A request arriving while a run is live answers
/// `started: false` and changes nothing; one arriving after an interrupted run resumes it,
/// because every committed batch stays committed.
#[utoipa::path(
    post,
    path = "/v1/admin/catalogue/purge",
    tag = ADMIN_CATALOGUE_TAG,
    request_body = PurgeRequest,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Whether a run was started, and the purge's state", body = PurgeStartView),
        (status = 400, description = "`confirm` does not echo the scope", body = crate::error::ProblemDetails),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "no second factor is enrolled, a step-up is required, or the caller does not hold the required permission", body = crate::error::ProblemDetails),
    )
)]
pub async fn purge_catalogue(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<PurgeRequest>,
) -> ApiResult<Json<PurgeStartView>> {
    user.require(Permission::CatalogueDelete).await?;

    if body.confirm.trim() != body.scope.token() {
        audit_failure(
            &state,
            &user,
            "catalogue.purge",
            body.scope.token(),
            &serde_json::json!({ "reason": "confirmation_mismatch" }),
        )
        .await;
        return Err(ApiError::BadRequest(format!(
            "confirm must be {:?} to purge that scope",
            body.scope.token()
        )));
    }

    let claim =
        purge_state::claim_purge(&state.pool, body.scope.target(), user.user_id.as_uuid()).await?;
    let started = claim.is_some();
    if let Some(claim) = claim {
        // Attributed now, while the request's actor and origin are at hand; the run records it
        // once it knows how the purge ended.
        let finished = user
            .event("catalogue.purge.finish")
            .target(body.scope.token());
        tokio::spawn(tankovault_service::in_current_trace(run(
            state.clone(),
            claim,
            body.scope,
            finished,
        )));
    }
    audit(
        &state,
        &user,
        "catalogue.purge",
        body.scope.token(),
        &serde_json::json!({ "started": started }),
    )
    .await;

    let status = purge_state::read_purge_state(&state.pool).await?.into();
    Ok(Json(PurgeStartView { started, status }))
}

/// Get the catalogue purge's state
///
/// Progress of the run `POST` of this path starts, and the outcome of the last one. Read from
/// the database, so any replica answers for the run wherever it is executing.
#[utoipa::path(
    get,
    path = "/v1/admin/catalogue/purge",
    tag = ADMIN_CATALOGUE_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "The purge's current state", body = PurgeStatusView),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "no second factor is enrolled, or the caller does not hold the required permission", body = crate::error::ProblemDetails),
    )
)]
pub async fn catalogue_purge_status(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<PurgeStatusView>> {
    user.require(Permission::CatalogueRead).await?;
    Ok(Json(
        purge_state::read_purge_state(&state.pool).await?.into(),
    ))
}

/// Cancel the catalogue purge
///
/// Asks the live run to stop after the batch it is on. Everything already removed stays
/// removed. A no-op when no run is live.
#[utoipa::path(
    post,
    path = "/v1/admin/catalogue/purge/cancel",
    tag = ADMIN_CATALOGUE_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "The purge's state after the request", body = PurgeStatusView),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "no second factor is enrolled, a step-up is required, or the caller does not hold the required permission", body = crate::error::ProblemDetails),
    )
)]
pub async fn cancel_catalogue_purge(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<PurgeStatusView>> {
    user.require(Permission::CatalogueDelete).await?;
    let requested = purge_state::request_purge_cancel(&state.pool).await?;
    audit(
        &state,
        &user,
        "catalogue.purge.cancel",
        "-",
        &serde_json::json!({ "requested": requested }),
    )
    .await;
    Ok(Json(
        purge_state::read_purge_state(&state.pool).await?.into(),
    ))
}

/// How a run ended.
struct Outcome {
    stop: PurgeStop,
    removed: DeletionReport,
    remaining: Option<i64>,
    error: Option<String>,
}

/// Drive the purge to its end, release the claim, and audit the result.
///
/// Every path out of [`drive`] reaches the release: a run that skipped it would hold the claim
/// until its lease expired, with the console showing a purge that is doing nothing.
async fn run(state: AppState, claim: PurgeClaim, scope: PurgeScope, event: AuditEvent) {
    tracing::info!(scope = scope.token(), "catalogue purge started");
    let outcome = drive(&state.pool, claim, scope).await;
    if let Err(e) = purge_state::finish_purge(
        &state.pool,
        claim,
        outcome.removed,
        outcome.remaining,
        outcome.stop,
        outcome.error.as_deref(),
    )
    .await
    {
        tracing::error!(error = %e, "failed to release the catalogue purge claim");
    }
    tracing::info!(
        scope = scope.token(),
        stopped = outcome.stop.as_str(),
        series = outcome.removed.series,
        chapters = outcome.removed.chapters,
        remaining = outcome.remaining,
        "catalogue purge finished"
    );
    let detail = serde_json::json!({
        "stopped": outcome.stop.as_str(),
        "removed": DeletionView::from(outcome.removed),
        "remaining": outcome.remaining,
        "error": outcome.error,
    });
    let result = if outcome.stop == PurgeStop::Failed {
        AuditOutcome::Failure
    } else {
        AuditOutcome::Success
    };
    state
        .audit
        .record(event.detail(detail).outcome(result))
        .await;
}

async fn drive(pool: &PgPool, claim: PurgeClaim, scope: PurgeScope) -> Outcome {
    let mut size = BatchSize::for_scope(scope);
    let mut outcome = Outcome {
        stop: PurgeStop::Failed,
        removed: DeletionReport::default(),
        remaining: None,
        error: None,
    };
    let mut failures = 0;
    loop {
        let began = Instant::now();
        let batch = match batch(pool, scope, size.get()).await {
            Ok(batch) => batch,
            Err(e) => {
                failures += 1;
                tracing::warn!(error = %e, failures, "catalogue purge batch failed");
                if failures >= MAX_BATCH_FAILURES {
                    outcome.error = Some(e.to_string());
                    return outcome;
                }
                size.shrink();
                tokio::time::sleep(RETRY_PAUSE).await;
                continue;
            }
        };
        failures = 0;
        size.observe(began.elapsed());
        let (report, left) = batch;
        outcome.removed += report;
        outcome.remaining = Some(left);

        let progressed = match scope {
            PurgeScope::Chapters => report.chapters > 0,
            PurgeScope::Everything => report.series > 0,
        };
        outcome.stop = if left == 0 {
            PurgeStop::Done
        } else if !progressed {
            PurgeStop::Stalled
        } else {
            match purge_state::advance_purge(pool, claim, outcome.removed, left).await {
                Ok(true) => continue,
                // A cancel, or a claim superseded after a lapsed lease; the release that follows
                // is fenced by the claim, so the second case writes nothing.
                Ok(false) => PurgeStop::Cancelled,
                Err(e) => {
                    outcome.error = Some(e.to_string());
                    PurgeStop::Failed
                }
            }
        };
        return outcome;
    }
}

/// One committed batch on a pooled connection, released before the next so a long purge never
/// pins one of the admin pool's few connections.
async fn batch(
    pool: &PgPool,
    scope: PurgeScope,
    size: i64,
) -> tankovault_db::DbResult<(DeletionReport, i64)> {
    let mut conn = pool.acquire().await?;
    match scope {
        PurgeScope::Chapters => maintenance::purge_chapters_batch(&mut conn, size).await,
        PurgeScope::Everything => maintenance::purge_series_batch(&mut conn, size).await,
    }
}

/// A batch size that halves when a batch overruns [`BATCH_TARGET`] and doubles when it finishes
/// well inside it.
///
/// Fixed sizes were wrong in both directions: what a series costs to delete depends on how much
/// hangs off it, which varies by orders of magnitude between deployments and between the old
/// and new ends of one catalogue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BatchSize {
    current: i64,
    min: i64,
    max: i64,
}

impl BatchSize {
    const fn for_scope(scope: PurgeScope) -> Self {
        match scope {
            PurgeScope::Chapters => Self {
                current: 5_000,
                min: 500,
                max: 100_000,
            },
            PurgeScope::Everything => Self {
                current: 100,
                min: 10,
                max: 2_000,
            },
        }
    }

    const fn get(self) -> i64 {
        self.current
    }

    fn shrink(&mut self) {
        self.current = (self.current / 2).max(self.min);
    }

    fn observe(&mut self, took: Duration) {
        if took > BATCH_TARGET * 2 {
            self.shrink();
        } else if took < BATCH_TARGET / 2 {
            self.current = (self.current * 2).min(self.max);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_slow_batch_halves_down_to_the_floor() {
        let mut size = BatchSize::for_scope(PurgeScope::Everything);
        for _ in 0..10 {
            size.observe(Duration::from_secs(30));
        }
        assert_eq!(size.get(), 10);
    }

    #[test]
    fn a_fast_batch_doubles_up_to_the_ceiling() {
        let mut size = BatchSize::for_scope(PurgeScope::Everything);
        size.observe(Duration::from_millis(100));
        assert_eq!(size.get(), 200);
        for _ in 0..10 {
            size.observe(Duration::from_millis(100));
        }
        assert_eq!(size.get(), 2_000);
    }

    #[test]
    fn a_batch_near_the_target_keeps_its_size() {
        let mut size = BatchSize::for_scope(PurgeScope::Chapters);
        size.observe(BATCH_TARGET);
        assert_eq!(size.get(), 5_000);
    }

    #[test]
    fn the_confirmation_token_is_the_stored_scope() {
        assert_eq!(PurgeScope::Chapters.token(), "chapters");
        assert_eq!(PurgeScope::Everything.token(), "everything");
    }
}
