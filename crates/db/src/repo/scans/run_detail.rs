//! Where one run's time went: its tasks, their stages, and the stage totals.

use crate::error::DbResult;
use serde_json::Value as Json;
use sqlx::PgExecutor;
use tankovault_domain::{ScanRunId, TaskState};
use time::OffsetDateTime;
use uuid::Uuid;

/// One task of a run, with its stage and the breakdown it recorded when it settled.
#[derive(Debug, Clone)]
pub struct TaskBreakdown {
    /// The task.
    pub id: Uuid,
    /// What the task was asked to do.
    pub kind: String,
    /// What it was pointed at, as the planner wrote it.
    pub target: Json,
    /// Where it is in its lifecycle.
    pub state: TaskState,
    /// Claims taken on it, so a retried task reads higher than one.
    pub attempts: i16,
    /// The worker holding it, `None` before the first claim.
    pub worker_id: Option<String>,
    /// Why it failed, `None` unless it did.
    pub error: Option<String>,
    /// The stage the task is in, or the one a settled task ended in. A failure's stage says
    /// whether the provider stopped answering or our own ingest rejected the page.
    pub stage: Option<String>,
    /// When it entered that stage.
    pub stage_at: Option<OffsetDateTime>,
    /// Progress inside the stage, where the stage counts anything.
    pub stage_done: Option<i32>,
    /// What that progress counts up to.
    pub stage_total: Option<i32>,
    /// What the stage is working against: a series path, a catalogue page.
    pub stage_detail: Option<String>,
    /// When the task was planned, `None` when the row does not record it.
    pub created_at: Option<OffsetDateTime>,
    /// When a worker last took it, `None` while queued.
    pub claimed_at: Option<OffsetDateTime>,
    /// When it settled, `None` while it has not.
    pub finished_at: Option<OffsetDateTime>,
    /// Milliseconds between creation and claim, the time nobody was working on it.
    pub wait_ms: Option<i32>,
    /// Milliseconds between claim and settle, the time someone was.
    pub duration_ms: Option<i32>,
    /// The [`tankovault_domain::StageTimings`] blob, as stored.
    pub telemetry: Option<Json>,
}

/// How a page of one run's tasks is ordered.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TaskSort {
    /// Costliest first, with tasks still running ahead of settled ones.
    ///
    /// The default because it is the question being asked: a run with 4,000 tasks has no useful
    /// "first" task, it has three that took a hundred times longer than the rest.
    #[default]
    Slowest,
    /// Most recently settled first, with the ones still running at the top.
    Recent,
}

impl TaskSort {
    /// Returns the token the statement's `ORDER BY` compares against.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Slowest => "slowest",
            Self::Recent => "recent",
        }
    }
}

/// Returns one run's tasks, ordered so the ones that explain its duration come first.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; a run with no tasks yet is an empty `Vec`.
pub async fn run_tasks<'e, E: PgExecutor<'e>>(
    exec: E,
    run_id: ScanRunId,
    sort: TaskSort,
    limit: i64,
) -> DbResult<Vec<TaskBreakdown>> {
    let rows = sqlx::query_as!(
        TaskBreakdown,
        "SELECT t.id, t.kind, t.target AS \"target: Json\", t.state AS \"state: TaskState\", \
                t.attempts, t.worker_id, t.error, \
                t.stage, t.stage_at, t.stage_done, t.stage_total, t.stage_detail, \
                t.created_at, t.claimed_at, t.finished_at, \
                t.wait_ms, t.duration_ms, t.telemetry AS \"telemetry: Json\" \
         FROM scan_tasks t \
         WHERE t.run_id = $1 \
         ORDER BY \
           (t.state = 'claimed') DESC, \
           CASE WHEN $2::text = 'recent' THEN t.finished_at END DESC NULLS FIRST, \
           t.duration_ms DESC NULLS LAST, \
           t.created_at DESC NULLS LAST \
         LIMIT $3",
        run_id.as_uuid(),
        sort.token(),
        limit,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

/// One run's time and request totals.
#[derive(Debug, Clone, Default)]
pub struct RunTelemetry {
    /// Settled tasks carrying a telemetry breakdown. `busy_ms` and `wait_ms` sum over every task
    /// of the run, measured or not; the fields after them sum over the breakdowns only.
    pub tasks_measured: i64,
    /// Summed execution time: work performed, not time elapsed, so it exceeds the run's wall
    /// clock whenever tasks ran concurrently.
    pub busy_ms: i64,
    /// Summed queue wait: time tasks spent created but unclaimed.
    pub wait_ms: i64,
    /// Outbound requests the run made.
    pub requests: i64,
    /// Milliseconds spent inside those requests.
    pub fetch_ms: i64,
    /// Time spent waiting for permission to send a request. Read against `busy_ms`, this is the
    /// figure that separates "the scan is slow" from "the provider's crawl budget is small".
    pub pace_wait_ms: i64,
    /// Milliseconds spent waiting on a challenge solver.
    pub solver_ms: i64,
    /// Solve attempts, whether or not they succeeded.
    pub solver_calls: i64,
    /// Responses the provider answered 429 or 503, each of which widened its spacing.
    pub throttled: i64,
}

/// Summed milliseconds for one stage across a run.
#[derive(Debug, Clone)]
pub struct StageTotal {
    /// A [`tankovault_domain::ScanStage`] token.
    pub stage: String,
    /// Milliseconds in this stage, summed across the run.
    pub millis: i64,
    /// Tasks that reported this stage at all.
    pub tasks: i64,
}

/// Returns the run's rollup and its per-stage split, as two statements.
///
/// Split because the second unnests a `jsonb` object per task, which leaves the rollup the
/// drawer shows first a plain aggregate.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; a run whose tasks recorded no breakdown yields zeroed breakdown
/// fields and an empty stage list, never [`crate::DbError::NotFound`].
pub async fn run_telemetry<'e, E: PgExecutor<'e> + Copy>(
    exec: E,
    run_id: ScanRunId,
) -> DbResult<(RunTelemetry, Vec<StageTotal>)> {
    // `->>` then a cast, not `->` with a jsonb cast: the values are JSON numbers written by
    // `serde_json`, and going through text is the one form that accepts every integer width
    // serde may have chosen.
    let rollup = sqlx::query_as!(
        RunTelemetry,
        "SELECT count(*) FILTER (WHERE t.telemetry IS NOT NULL) AS \"tasks_measured!\", \
                COALESCE(sum(t.duration_ms), 0)::int8 AS \"busy_ms!\", \
                COALESCE(sum(t.wait_ms), 0)::int8 AS \"wait_ms!\", \
                COALESCE(sum((t.telemetry->>'requests')::int8), 0)::int8 AS \"requests!\", \
                COALESCE(sum((t.telemetry->>'fetch_ms')::int8), 0)::int8 AS \"fetch_ms!\", \
                COALESCE(sum((t.telemetry->>'pace_wait_ms')::int8), 0)::int8 \
                    AS \"pace_wait_ms!\", \
                COALESCE(sum((t.telemetry->>'solver_ms')::int8), 0)::int8 AS \"solver_ms!\", \
                COALESCE(sum((t.telemetry->>'solver_calls')::int8), 0)::int8 \
                    AS \"solver_calls!\", \
                COALESCE(sum((t.telemetry->>'throttled')::int8), 0)::int8 AS \"throttled!\" \
         FROM scan_tasks t WHERE t.run_id = $1",
        run_id.as_uuid(),
    )
    .fetch_one(exec)
    .await?;

    let stages = sqlx::query_as!(
        StageTotal,
        "SELECT s.key AS \"stage!\", \
                sum(s.value::text::int8)::int8 AS \"millis!\", \
                count(*) AS \"tasks!\" \
         FROM scan_tasks t \
         CROSS JOIN LATERAL jsonb_each(t.telemetry->'stages') s \
         WHERE t.run_id = $1 AND jsonb_typeof(t.telemetry->'stages') = 'object' \
         GROUP BY s.key \
         ORDER BY sum(s.value::text::int8) DESC",
        run_id.as_uuid(),
    )
    .fetch_all(exec)
    .await?;

    Ok((rollup, stages))
}
