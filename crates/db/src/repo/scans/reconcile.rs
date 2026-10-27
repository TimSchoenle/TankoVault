//! Reconciliation between the task table and `JetStream`.
//!
//! `JetStream` is the truth for dispatch and this table is the truth for progress, and nothing
//! keeps the two in step: a publish that never landed, a stream that was purged or recreated, a
//! message acked by a worker that then died — each leaves a task row that is open forever and a
//! run that can never settle. Nothing retries them, because from the database's side there is no
//! failure to see. These four queries are what the reconciler compares and repairs against.

use super::runs::FAILURE_STREAK_WINDOW;
use crate::error::DbResult;
use serde_json::Value as Json;
use sqlx::PgExecutor;
use tankovault_domain::{ProviderId, ScanMode, ScanRunId, ScanTaskId};

/// One provider lane's open work, as the database sees it.
#[derive(Debug, Clone)]
pub struct OpenLane {
    /// The provider whose lane this is.
    pub provider_id: ProviderId,
    /// Its slug, which is the last token of the lane's subject.
    pub provider_slug: String,
    /// The cadence. A provider has one lane per cadence, not one in total.
    pub mode: ScanMode,
    /// Tasks still open (`queued`/`claimed`/`running`) in runs that have not settled.
    pub open_tasks: i64,
    /// Of those, the ones old enough that a lost publish explains them better than a race with
    /// one — a task row is committed before its message is published, so a fresh row with no
    /// message is the ordinary intermediate state, not a fault.
    pub aged_tasks: i64,
}

/// A task the reconciler may republish: everything a `ScanTaskMessage` needs that the lane does
/// not already carry.
#[derive(Debug, Clone)]
pub struct StrandedTask {
    /// The task to republish.
    pub id: ScanTaskId,
    /// The run it belongs to.
    pub run_id: ScanRunId,
    /// Which adapter entry point the message should name.
    pub kind: String,
    /// The target to carry on it, unchanged from the row.
    pub target: Json,
}

/// Every provider lane that has open work, with how much of it is old enough to be repaired.
///
/// Driven from `scan_runs` rather than from `scan_tasks`: the active runs are a handful of rows
/// behind `scan_runs_active_provider_mode`, and each one's task count is an index lookup on
/// `scan_tasks_run_state`. The natural spelling — filter `scan_tasks` by state and group up —
/// reads a table that grows by a row per series per scan, on a schedule.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only. No open lanes is an empty `Vec`, which is the steady state.
pub async fn open_task_lanes<'e, E: PgExecutor<'e>>(
    exec: E,
    aged_after: std::time::Duration,
) -> DbResult<Vec<OpenLane>> {
    let rows = sqlx::query!(
        "SELECT p.id AS \"provider_id!\", p.slug AS \"provider_slug!\", \
                r.mode AS \"mode!: ScanMode\", \
                sum(c.open_tasks)::int8 AS \"open_tasks!\", \
                sum(c.aged_tasks)::int8 AS \"aged_tasks!\" \
         FROM scan_runs r \
         JOIN providers p ON p.id = r.provider_id \
         CROSS JOIN LATERAL ( \
             SELECT count(*) AS open_tasks, \
                    count(*) FILTER ( \
                        WHERE t.created_at IS NULL \
                           OR t.created_at < now() - make_interval(secs => $1) \
                    ) AS aged_tasks \
             FROM scan_tasks t \
             WHERE t.run_id = r.id AND t.state IN ('queued','claimed','running') \
         ) c \
         WHERE r.state IN ('queued','running') \
         GROUP BY p.id, p.slug, r.mode \
         HAVING sum(c.open_tasks) > 0",
        aged_after.as_secs_f64(),
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| OpenLane {
            provider_id: ProviderId::from_uuid(r.provider_id),
            provider_slug: r.provider_slug,
            mode: r.mode,
            open_tasks: r.open_tasks,
            aged_tasks: r.aged_tasks,
        })
        .collect())
}

/// The oldest open tasks in one lane, which are the ones a short broker backlog has stranded.
///
/// Oldest first because the deficit is counted, not identified: the broker says how many
/// messages it holds for the lane, not which. The oldest rows are the ones a message is least
/// likely to still exist for, and republishing one that does exist is harmless — the second
/// delivery finds the task claimed or settled and is declined.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn stranded_tasks<'e, E: PgExecutor<'e>>(
    exec: E,
    provider_id: ProviderId,
    mode: ScanMode,
    aged_after: std::time::Duration,
    limit: i64,
) -> DbResult<Vec<StrandedTask>> {
    let rows = sqlx::query!(
        "SELECT t.id, t.run_id, t.kind, t.target \
         FROM scan_tasks t \
         JOIN scan_runs r ON r.id = t.run_id \
         WHERE r.provider_id = $1 AND r.mode = $2::scan_mode \
           AND r.state IN ('queued','running') \
           AND t.state IN ('queued','claimed','running') \
           AND (t.created_at IS NULL OR t.created_at < now() - make_interval(secs => $3)) \
         ORDER BY t.created_at ASC NULLS FIRST \
         LIMIT $4",
        provider_id.as_uuid(),
        mode as ScanMode,
        aged_after.as_secs_f64(),
        limit,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| StrandedTask {
            id: ScanTaskId::from_uuid(r.id),
            run_id: ScanRunId::from_uuid(r.run_id),
            kind: r.kind,
            target: r.target,
        })
        .collect())
}

/// Runs whose tasks have all settled but which are still `running`.
///
/// Finalisation rides on a progress event, and that event is best-effort: one lost publish
/// leaves a run that is finished in every respect sitting open forever, counted as active by the
/// console and — until it goes stale — suppressing the provider's next run of the same mode.
/// Nothing else ever revisits such a run, because no further task will settle on it.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn runs_awaiting_finalisation<'e, E: PgExecutor<'e>>(
    exec: E,
    limit: i64,
) -> DbResult<Vec<ScanRunId>> {
    let rows = sqlx::query_scalar!(
        "SELECT id FROM scan_runs \
         WHERE state = 'running' AND total_tasks > 0 \
           AND (done_tasks + failed_tasks) >= total_tasks \
         ORDER BY created_at ASC \
         LIMIT $1",
        limit,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows.into_iter().map(ScanRunId::from_uuid).collect())
}

/// Fail runs that were opened but never planned a single task, and return the ones failed.
///
/// The planner creates the run row, then the task, then publishes. A process killed between the
/// first two steps leaves a run with no tasks at all — nothing to republish and nothing that can
/// ever settle it, so it is neither a lane this reconciler sees nor a run
/// [`runs_awaiting_finalisation`] can close. `older_than` is what separates it from a plan that
/// is merely a few milliseconds into that same sequence.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn fail_unplanned_runs<'e, E: PgExecutor<'e>>(
    exec: E,
    older_than: std::time::Duration,
    limit: i64,
) -> DbResult<Vec<ScanRunId>> {
    let rows = sqlx::query_scalar!(
        "UPDATE scan_runs SET state = 'failed', finished_at = now() \
         WHERE id IN ( \
             SELECT r.id FROM scan_runs r \
             WHERE r.state IN ('queued','running') AND r.total_tasks = 0 \
               AND r.created_at < now() - make_interval(secs => $1) \
               AND NOT EXISTS (SELECT 1 FROM scan_tasks t WHERE t.run_id = r.id) \
             ORDER BY r.created_at ASC \
             LIMIT $2 \
         ) \
         RETURNING id",
        older_than.as_secs_f64(),
        limit,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows.into_iter().map(ScanRunId::from_uuid).collect())
}

/// What one [`prune_scan_history`] pass deleted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HistoryPrune {
    /// Task rows removed.
    pub tasks: u64,
    /// Run rows removed.
    pub runs: u64,
}

impl HistoryPrune {
    /// Whether the pass found nothing left to remove.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.tasks == 0 && self.runs == 0
    }
}

/// Delete one bounded batch of settled scan history older than `retention_days`.
///
/// A run goes only once it is terminal and past the cutoff, its tasks first and in batches of
/// `task_batch`, the run row once none are left — so a run with fifty thousand tasks takes
/// several passes rather than one long `DELETE`. Call it until [`HistoryPrune::is_empty`].
///
/// **The newest 32 finished runs of each provider and mode are kept,
/// however old.** [`failure_streak`] reads exactly that window; pruning into it would shorten a
/// failing provider's streak and cut its backoff, which is the only thing keeping the scheduler
/// from hammering a site that answers nothing. It also keeps every provider's latest run, which
/// the provider table reports.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; nothing to prune is an empty [`HistoryPrune`].
///
/// [`failure_streak`]: super::failure_streak
pub async fn prune_scan_history(
    pool: &sqlx::PgPool,
    retention_days: u32,
    run_batch: i64,
    task_batch: i64,
) -> DbResult<HistoryPrune> {
    let runs = sqlx::query_scalar!(
        "SELECT r.id FROM scan_runs r \
         WHERE r.state IN ('completed','failed','cancelled') \
           AND r.created_at < now() - make_interval(days => $1) \
           AND (r.provider_id IS NULL OR ( \
                 SELECT count(*) FROM ( \
                   SELECT 1 FROM scan_runs n \
                   WHERE n.provider_id = r.provider_id AND n.mode = r.mode \
                     AND n.state IN ('completed','failed') AND n.created_at > r.created_at \
                   LIMIT $3 \
                 ) newer) >= $3) \
         ORDER BY r.created_at ASC \
         LIMIT $2",
        i32::try_from(retention_days).unwrap_or(i32::MAX),
        run_batch,
        FAILURE_STREAK_WINDOW,
    )
    .fetch_all(pool)
    .await?;
    if runs.is_empty() {
        return Ok(HistoryPrune::default());
    }

    let tasks = sqlx::query!(
        "DELETE FROM scan_tasks WHERE id IN ( \
             SELECT id FROM scan_tasks WHERE run_id = ANY($1) LIMIT $2 \
         )",
        &runs,
        task_batch,
    )
    .execute(pool)
    .await?
    .rows_affected();

    let deleted_runs = sqlx::query!(
        "DELETE FROM scan_runs r \
         WHERE r.id = ANY($1) \
           AND NOT EXISTS (SELECT 1 FROM scan_tasks t WHERE t.run_id = r.id)",
        &runs,
    )
    .execute(pool)
    .await?
    .rows_affected();

    Ok(HistoryPrune {
        tasks,
        runs: deleted_runs,
    })
}
