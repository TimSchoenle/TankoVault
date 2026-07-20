//! Scan run + task repository. The task row is the truth for **progress and audit**;
//! the `JetStream` stream is the truth for **dispatch** (design §2). A durable
//! `SELECT ... FOR UPDATE SKIP LOCKED` claim path is provided as the fallback/audit
//! mechanism when the broker is unavailable.

use crate::error::{DbError, DbResult};
use tankovault_domain::{
    ProviderId, RunState, ScanMode, ScanRun, ScanRunId, ScanTask, ScanTaskId, TaskState,
};
use serde_json::Value as Json;
use sqlx::{FromRow, PgExecutor};
use std::str::FromStr;
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(FromRow)]
struct RunRow {
    id: Uuid,
    provider_id: Option<Uuid>,
    mode: String,
    state: String,
    total_tasks: i32,
    done_tasks: i32,
    failed_tasks: i32,
    started_at: Option<OffsetDateTime>,
    finished_at: Option<OffsetDateTime>,
    created_at: OffsetDateTime,
}

impl TryFrom<RunRow> for ScanRun {
    type Error = DbError;
    fn try_from(r: RunRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: ScanRunId::from_uuid(r.id),
            provider_id: r.provider_id.map(ProviderId::from_uuid),
            mode: ScanMode::from_str(&r.mode)?,
            state: RunState::from_str(&r.state)?,
            total_tasks: r.total_tasks,
            done_tasks: r.done_tasks,
            failed_tasks: r.failed_tasks,
            started_at: r.started_at,
            finished_at: r.finished_at,
            created_at: r.created_at,
        })
    }
}

/// `scan_runs` column projection as a literal.
macro_rules! run_cols {
    () => {
        "id, provider_id, mode::text AS mode, state::text AS state, total_tasks, \
         done_tasks, failed_tasks, started_at, finished_at, created_at"
    };
}

/// Create a queued scan run.
pub async fn create_run<'e, E: PgExecutor<'e>>(
    exec: E,
    provider_id: Option<ProviderId>,
    mode: ScanMode,
) -> DbResult<ScanRunId> {
    let id = ScanRunId::new();
    sqlx::query("INSERT INTO scan_runs (id, provider_id, mode) VALUES ($1,$2,$3::scan_mode)")
        .bind(id.as_uuid())
        .bind(provider_id.map(ProviderId::as_uuid))
        .bind(mode.as_str())
        .execute(exec)
        .await?;
    Ok(id)
}

/// Fetch a run by id.
pub async fn get_run<'e, E: PgExecutor<'e>>(exec: E, id: ScanRunId) -> DbResult<ScanRun> {
    let row: Option<RunRow> = sqlx::query_as(concat!(
        "SELECT ",
        run_cols!(),
        " FROM scan_runs WHERE id = $1"
    ))
    .bind(id.as_uuid())
    .fetch_optional(exec)
    .await?;
    row.ok_or(DbError::NotFound)?.try_into()
}

/// List recent runs (console overview).
pub async fn list_recent_runs<'e, E: PgExecutor<'e>>(
    exec: E,
    limit: i64,
) -> DbResult<Vec<ScanRun>> {
    let rows: Vec<RunRow> = sqlx::query_as(concat!(
        "SELECT ",
        run_cols!(),
        " FROM scan_runs ORDER BY created_at DESC LIMIT $1"
    ))
    .bind(limit)
    .fetch_all(exec)
    .await?;
    rows.into_iter().map(ScanRun::try_from).collect()
}

/// Transition a run to `running` and stamp `started_at`.
pub async fn start_run<'e, E: PgExecutor<'e>>(exec: E, id: ScanRunId) -> DbResult<()> {
    sqlx::query(
        "UPDATE scan_runs SET state = 'running', started_at = COALESCE(started_at, now()) \
         WHERE id = $1",
    )
    .bind(id.as_uuid())
    .execute(exec)
    .await?;
    Ok(())
}

/// Set a run's final state and stamp `finished_at`.
pub async fn finish_run<'e, E: PgExecutor<'e>>(
    exec: E,
    id: ScanRunId,
    state: RunState,
) -> DbResult<()> {
    sqlx::query("UPDATE scan_runs SET state = $2::run_state, finished_at = now() WHERE id = $1")
        .bind(id.as_uuid())
        .bind(state.as_str())
        .execute(exec)
        .await?;
    Ok(())
}

/// Atomically finalise a run **iff** it is still `running` and every planned task has
/// settled (`done_tasks + failed_tasks >= total_tasks`, with `total_tasks > 0`). The
/// terminal state is [`RunState::Failed`] only when every task failed, otherwise
/// [`RunState::Completed`] (a partially-failed run still completed). Returns the updated
/// [`ScanRun`] when this call performed the transition, or `None` when the run was not
/// yet complete or another actor already finalised it — so the progress aggregator emits
/// exactly one terminal event and cannot loop (design §8, §12).
pub async fn finalize_if_complete<'e, E: PgExecutor<'e>>(
    exec: E,
    id: ScanRunId,
) -> DbResult<Option<ScanRun>> {
    let row: Option<RunRow> = sqlx::query_as(concat!(
        "UPDATE scan_runs SET \
            state = CASE WHEN done_tasks = 0 AND failed_tasks > 0 \
                         THEN 'failed'::run_state ELSE 'completed'::run_state END, \
            finished_at = now() \
         WHERE id = $1 AND state = 'running' AND total_tasks > 0 \
               AND (done_tasks + failed_tasks) >= total_tasks \
         RETURNING ",
        run_cols!()
    ))
    .bind(id.as_uuid())
    .fetch_optional(exec)
    .await?;
    row.map(ScanRun::try_from).transpose()
}

/// Add to a run's planned task total (as the planner fans out).
pub async fn add_total_tasks<'e, E: PgExecutor<'e>>(
    exec: E,
    id: ScanRunId,
    delta: i32,
) -> DbResult<()> {
    sqlx::query("UPDATE scan_runs SET total_tasks = total_tasks + $2 WHERE id = $1")
        .bind(id.as_uuid())
        .bind(delta)
        .execute(exec)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tasks
// ---------------------------------------------------------------------------

#[derive(FromRow)]
struct TaskRow {
    id: Uuid,
    run_id: Uuid,
    kind: String,
    target: Json,
    state: String,
    attempts: i16,
    worker_id: Option<String>,
    error: Option<String>,
    claimed_at: Option<OffsetDateTime>,
    finished_at: Option<OffsetDateTime>,
}

impl TryFrom<TaskRow> for ScanTask {
    type Error = DbError;
    fn try_from(r: TaskRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: ScanTaskId::from_uuid(r.id),
            run_id: ScanRunId::from_uuid(r.run_id),
            kind: r.kind,
            target: r.target,
            state: TaskState::from_str(&r.state)?,
            attempts: r.attempts,
            worker_id: r.worker_id,
            error: r.error,
            claimed_at: r.claimed_at,
            finished_at: r.finished_at,
        })
    }
}

/// Create a queued task and return its id (planner writes then publishes to `JetStream`).
pub async fn create_task<'e, E: PgExecutor<'e>>(
    exec: E,
    run_id: ScanRunId,
    kind: &str,
    target: &Json,
) -> DbResult<ScanTaskId> {
    let id = ScanTaskId::new();
    sqlx::query("INSERT INTO scan_tasks (id, run_id, kind, target) VALUES ($1,$2,$3,$4)")
        .bind(id.as_uuid())
        .bind(run_id.as_uuid())
        .bind(kind)
        .bind(target)
        .execute(exec)
        .await?;
    Ok(id)
}

/// Mark a task claimed by a worker (the normal path: the worker already has the message
/// from `JetStream` and records the claim for audit/progress).
pub async fn claim_task<'e, E: PgExecutor<'e>>(
    exec: E,
    task_id: ScanTaskId,
    worker_id: &str,
) -> DbResult<()> {
    sqlx::query(
        "UPDATE scan_tasks SET state = 'claimed', worker_id = $2, claimed_at = now(), \
         attempts = attempts + 1 WHERE id = $1",
    )
    .bind(task_id.as_uuid())
    .bind(worker_id)
    .execute(exec)
    .await?;
    Ok(())
}

/// Durable fallback claim: atomically grab the oldest queued task for a run using
/// `FOR UPDATE SKIP LOCKED`, used when the broker is unavailable.
pub async fn claim_next_queued<'e, E: PgExecutor<'e>>(
    exec: E,
    run_id: ScanRunId,
    worker_id: &str,
) -> DbResult<Option<ScanTask>> {
    let row: Option<TaskRow> = sqlx::query_as(
        "UPDATE scan_tasks SET state = 'claimed', worker_id = $2, claimed_at = now(), \
             attempts = attempts + 1 \
         WHERE id = ( \
            SELECT id FROM scan_tasks WHERE run_id = $1 AND state = 'queued' \
            ORDER BY id FOR UPDATE SKIP LOCKED LIMIT 1 \
         ) \
         RETURNING id, run_id, kind, target, state::text AS state, attempts, worker_id, \
                   error, claimed_at, finished_at",
    )
    .bind(run_id.as_uuid())
    .bind(worker_id)
    .fetch_optional(exec)
    .await?;
    row.map(ScanTask::try_from).transpose()
}

/// Mark a task done and increment the run's done counter, atomically per statement.
pub async fn complete_task<'e, E: PgExecutor<'e>>(exec: E, task_id: ScanTaskId) -> DbResult<()> {
    // Two statements would race on the counter under concurrency; a CTE keeps them in
    // one round trip and lets each worker's increment commit independently.
    sqlx::query(
        "WITH done AS ( \
            UPDATE scan_tasks SET state = 'done', finished_at = now() \
            WHERE id = $1 AND state <> 'done' RETURNING run_id \
         ) \
         UPDATE scan_runs SET done_tasks = done_tasks + 1 \
         WHERE id = (SELECT run_id FROM done)",
    )
    .bind(task_id.as_uuid())
    .execute(exec)
    .await?;
    Ok(())
}

/// Mark a task failed with an error, incrementing the run's failed counter.
pub async fn fail_task<'e, E: PgExecutor<'e>>(
    exec: E,
    task_id: ScanTaskId,
    error: &str,
) -> DbResult<()> {
    sqlx::query(
        "WITH failed AS ( \
            UPDATE scan_tasks SET state = 'failed', error = $2, finished_at = now() \
            WHERE id = $1 AND state <> 'failed' RETURNING run_id \
         ) \
         UPDATE scan_runs SET failed_tasks = failed_tasks + 1 \
         WHERE id = (SELECT run_id FROM failed)",
    )
    .bind(task_id.as_uuid())
    .bind(error)
    .execute(exec)
    .await?;
    Ok(())
}

/// A failed scan task enriched with its run's provider + mode, for the console error feed.
#[derive(Debug, Clone, serde::Serialize, FromRow)]
pub struct FailedTaskView {
    pub id: Uuid,
    pub run_id: Uuid,
    /// Provider slug of the owning run (`None` if the provider was since deleted).
    pub provider_slug: Option<String>,
    pub mode: String,
    pub kind: String,
    pub error: Option<String>,
    pub attempts: i16,
    #[serde(with = "time::serde::rfc3339::option")]
    pub finished_at: Option<OffsetDateTime>,
}

/// The most recently failed tasks across all runs, newest first — the operator's triage
/// feed for stuck providers / broken selectors (design §17.2.7).
pub async fn recent_failed_tasks<'e, E: PgExecutor<'e>>(
    exec: E,
    limit: i64,
) -> DbResult<Vec<FailedTaskView>> {
    let rows: Vec<FailedTaskView> = sqlx::query_as(
        "SELECT t.id, t.run_id, p.slug AS provider_slug, r.mode::text AS mode, \
                t.kind, t.error, t.attempts, t.finished_at \
         FROM scan_tasks t \
         JOIN scan_runs r ON r.id = t.run_id \
         LEFT JOIN providers p ON p.id = r.provider_id \
         WHERE t.state = 'failed' \
         ORDER BY t.finished_at DESC NULLS LAST \
         LIMIT $1",
    )
    .bind(limit)
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

/// Mark a task skipped (unchanged content, no work needed) and count it as done.
pub async fn skip_task<'e, E: PgExecutor<'e>>(exec: E, task_id: ScanTaskId) -> DbResult<()> {
    sqlx::query(
        "WITH skipped AS ( \
            UPDATE scan_tasks SET state = 'skipped', finished_at = now() \
            WHERE id = $1 AND state NOT IN ('done','skipped') RETURNING run_id \
         ) \
         UPDATE scan_runs SET done_tasks = done_tasks + 1 \
         WHERE id = (SELECT run_id FROM skipped)",
    )
    .bind(task_id.as_uuid())
    .execute(exec)
    .await?;
    Ok(())
}
