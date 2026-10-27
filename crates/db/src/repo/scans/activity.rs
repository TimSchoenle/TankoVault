//! Window health and live activity: what the scan panel reports across runs.

use crate::error::DbResult;
use serde_json::Value as Json;
use sqlx::PgExecutor;
use tankovault_domain::TaskState;
use time::OffsetDateTime;
use uuid::Uuid;

/// What the scan panel's filter actually matched, as figures rather than rows.
///
/// The console's filter used to change only which rows were listed, which answers "what
/// happened" and never "how is it going". These are the counts the same predicate produces over
/// the whole window, so a narrowed filter reports its own success rate rather than the
/// deployment's.
#[derive(Debug, Clone)]
pub struct ScanSummary {
    /// Runs the filter matched.
    pub runs_total: i64,
    /// Of those, still waiting to start.
    pub runs_queued: i64,
    /// Of those, in flight.
    pub runs_running: i64,
    /// Of those, settled with at least one task done.
    pub runs_completed: i64,
    /// Of those, settled with every task failed, which is what marks a run failed rather
    /// than completed.
    pub runs_failed: i64,
    /// Of those, stopped by an operator.
    pub runs_cancelled: i64,
    /// Tasks those runs dispatched.
    pub tasks_total: i64,
    /// Of those, settled successfully.
    pub tasks_done: i64,
    /// Of those, settled as failures.
    pub tasks_failed: i64,
    /// Failures still in the triage feed — what an operator has left to look at, as opposed to
    /// `tasks_failed`, which counts everything that ever failed in the window.
    pub failures_open: i64,
    /// Summed run wall-clock in seconds. A run still in flight counts up to `now()`, which is
    /// what makes throughput derived from this move while a scan is running.
    pub busy_seconds: f64,
    /// Creation time of the oldest matching run, `None` when nothing matched.
    pub first_run_at: Option<OffsetDateTime>,
    /// Creation time of the newest matching run, `None` when nothing matched.
    pub last_run_at: Option<OffsetDateTime>,
}

/// The window rollup for the scan panel's health bar.
///
/// The failure count is bounded by `finished_at` while the run counts are bounded by
/// `created_at`, deliberately: they are the columns each of those things actually happened at,
/// and it keeps this consistent with the failure feed, which windows on `finished_at` too.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only. A window matching nothing is a zeroed summary, never
/// [`crate::DbError::NotFound`] — an idle deployment is a legitimate answer here.
pub async fn scan_summary<'e, E: PgExecutor<'e>>(
    exec: E,
    provider: Option<&str>,
    since: Option<OffsetDateTime>,
) -> DbResult<ScanSummary> {
    let row = sqlx::query_as!(
        ScanSummary,
        "WITH matched AS ( \
             SELECT r.state, r.total_tasks, r.done_tasks, r.failed_tasks, \
                    r.started_at, r.finished_at, r.created_at \
             FROM scan_runs r \
             LEFT JOIN providers p ON p.id = r.provider_id \
             WHERE ($1::text IS NULL OR p.slug = $1) \
               AND ($2::timestamptz IS NULL OR r.created_at >= $2) \
         ), open AS ( \
             SELECT count(*) AS n \
             FROM scan_tasks t \
             JOIN scan_runs r ON r.id = t.run_id \
             LEFT JOIN providers p ON p.id = r.provider_id \
             WHERE t.state = 'failed' AND t.acknowledged_at IS NULL \
               AND ($1::text IS NULL OR p.slug = $1) \
               AND ($2::timestamptz IS NULL OR t.finished_at >= $2) \
         ) \
         SELECT count(*) AS \"runs_total!\", \
                count(*) FILTER (WHERE state = 'queued') AS \"runs_queued!\", \
                count(*) FILTER (WHERE state = 'running') AS \"runs_running!\", \
                count(*) FILTER (WHERE state = 'completed') AS \"runs_completed!\", \
                count(*) FILTER (WHERE state = 'failed') AS \"runs_failed!\", \
                count(*) FILTER (WHERE state = 'cancelled') AS \"runs_cancelled!\", \
                COALESCE(sum(total_tasks), 0) AS \"tasks_total!\", \
                COALESCE(sum(done_tasks), 0) AS \"tasks_done!\", \
                COALESCE(sum(failed_tasks), 0) AS \"tasks_failed!\", \
                (SELECT n FROM open) AS \"failures_open!\", \
                COALESCE(sum(EXTRACT(EPOCH FROM (COALESCE(finished_at, now()) - started_at))), 0) \
                    ::float8 AS \"busy_seconds!\", \
                min(created_at) AS first_run_at, \
                max(created_at) AS last_run_at \
         FROM matched",
        provider,
        since,
    )
    .fetch_one(exec)
    .await?;
    Ok(row)
}

/// One provider's scan health over the window.
#[derive(Debug, Clone)]
pub struct ProviderScanHealth {
    /// The provider, as the console filter spells it.
    pub slug: String,
    /// Its display name.
    pub name: String,
    /// Runs it had in the window.
    pub runs: i64,
    /// Of those, still queued or running.
    pub runs_active: i64,
    /// Of those, failed.
    pub runs_failed: i64,
    /// Tasks of those runs that settled successfully.
    pub tasks_done: i64,
    /// Tasks of those runs that failed.
    pub tasks_failed: i64,
    /// Failures still in the triage feed for this provider.
    pub failures_open: i64,
    /// Its most recent run in the window, `None` when it had none.
    pub last_run_at: Option<OffsetDateTime>,
    /// Its most recent failure in the window, `None` when it had none.
    pub last_failure_at: Option<OffsetDateTime>,
}

/// Per-provider scan health, worst first.
///
/// Providers with neither a run in the window nor an open failure are omitted rather than listed
/// as zeroes: the table is a place to look when something is wrong, and a deployment's full
/// provider list is what the Providers panel is for.
///
/// Two grouped passes joined to `providers`, not a lateral per provider: the lateral re-joined
/// the failure history to `scan_runs` once for every provider, which production logged at
/// 19.8–29.4 s. On 102 providers and 274 000 failed tasks it measured 70 ms warm and 423 ms cold
/// (174 ms of that JIT, estimated cost 1.53 M); grouped, 35 ms and 54 ms at a cost of 17 800.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; a quiet window is an empty `Vec`.
pub async fn provider_scan_health<'e, E: PgExecutor<'e>>(
    exec: E,
    since: Option<OffsetDateTime>,
    limit: i64,
) -> DbResult<Vec<ProviderScanHealth>> {
    let rows = sqlx::query_as!(
        ProviderScanHealth,
        "WITH runs AS ( \
             SELECT r.provider_id, \
                    count(*) AS runs, \
                    count(*) FILTER (WHERE r.state IN ('queued','running')) AS runs_active, \
                    count(*) FILTER (WHERE r.state = 'failed') AS runs_failed, \
                    sum(r.done_tasks) AS tasks_done, \
                    sum(r.failed_tasks) AS tasks_failed, \
                    max(r.created_at) AS last_run_at \
             FROM scan_runs r \
             WHERE r.provider_id IS NOT NULL \
               AND ($1::timestamptz IS NULL OR r.created_at >= $1) \
             GROUP BY r.provider_id \
         ), open AS ( \
             SELECT r.provider_id, count(*) AS open_count, max(t.finished_at) AS latest_at \
             FROM scan_tasks t \
             JOIN scan_runs r ON r.id = t.run_id \
             WHERE t.state = 'failed' AND t.acknowledged_at IS NULL \
               AND ($1::timestamptz IS NULL OR t.finished_at >= $1) \
               AND r.provider_id IS NOT NULL \
             GROUP BY r.provider_id \
         ) \
         SELECT p.slug AS \"slug!\", p.name AS \"name!\", \
                COALESCE(runs.runs, 0) AS \"runs!\", \
                COALESCE(runs.runs_active, 0) AS \"runs_active!\", \
                COALESCE(runs.runs_failed, 0) AS \"runs_failed!\", \
                COALESCE(runs.tasks_done, 0) AS \"tasks_done!\", \
                COALESCE(runs.tasks_failed, 0) AS \"tasks_failed!\", \
                COALESCE(open.open_count, 0) AS \"failures_open!\", \
                runs.last_run_at AS last_run_at, \
                open.latest_at AS last_failure_at \
         FROM providers p \
         LEFT JOIN runs ON runs.provider_id = p.id \
         LEFT JOIN open ON open.provider_id = p.id \
         WHERE runs.provider_id IS NOT NULL OR open.provider_id IS NOT NULL \
         ORDER BY COALESCE(open.open_count, 0) DESC, \
                  COALESCE(runs.tasks_failed, 0) DESC, \
                  runs.last_run_at DESC NULLS LAST, \
                  p.slug \
         LIMIT $2",
        since,
        limit,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

/// One in-flight run's task breakdown, as the live console panel reads it.
pub struct RunActivity {
    /// The run.
    pub run_id: Uuid,
    /// Tasks nobody has claimed yet.
    pub queued_tasks: i64,
    /// Tasks a worker is holding right now.
    pub running_tasks: i64,
    /// When the oldest still-held task was claimed. A claim that stops moving is the first
    /// visible symptom of a wedged worker, and nothing else on this surface shows it.
    pub oldest_claim_at: Option<OffsetDateTime>,
    /// Distinct kinds in flight, sorted.
    pub kinds: Vec<String>,
    /// Distinct workers holding a task right now.
    pub workers: i64,
    /// The stage of the run's **oldest** held task — the one deciding how long the run takes.
    ///
    /// Picked by claim order rather than, say, the most common stage: a run's wall clock is set by
    /// whatever has been running longest, and that is the task an operator is asking about.
    pub stage: Option<String>,
    /// When that task entered the stage above, which is what makes "stuck in `series_chapters`
    /// for nine minutes" readable at all.
    pub stage_at: Option<OffsetDateTime>,
    /// Progress inside that stage, where the stage counts anything.
    pub stage_done: Option<i32>,
    /// What that progress counts up to.
    pub stage_total: Option<i32>,
    /// What that stage is working against — a series path, a catalogue page.
    pub stage_detail: Option<String>,
    /// When the run's oldest still-queued task was created.
    ///
    /// Read together with `running_tasks = 0`, this is the answer to the console's worst
    /// ambiguity: a run in `running` with nothing claimed is not working, it is **waiting for a
    /// worker slot** — a worker serves one task per provider, so a provider's second run queues
    /// behind its first. Both look identical through `scan_runs.state` alone, and the second one
    /// reads as a run that has hung.
    pub waiting_since: Option<OffsetDateTime>,
}

/// Per-run task activity for every run that has not reached a terminal state.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; an idle deployment is an empty `Vec`.
pub async fn active_run_activity<'e, E: PgExecutor<'e>>(exec: E) -> DbResult<Vec<RunActivity>> {
    let rows = sqlx::query_as!(
        RunActivity,
        "SELECT r.id AS \"run_id!\", \
                count(t.id) FILTER (WHERE t.state = 'queued') AS \"queued_tasks!\", \
                count(t.id) FILTER (WHERE t.state = 'claimed') AS \"running_tasks!\", \
                min(t.claimed_at) FILTER (WHERE t.state = 'claimed') AS oldest_claim_at, \
                COALESCE( \
                    array_agg(DISTINCT t.kind) FILTER (WHERE t.state = 'claimed'), \
                    ARRAY[]::text[] \
                ) AS \"kinds!\", \
                count(DISTINCT t.worker_id) FILTER (WHERE t.state = 'claimed') AS \"workers!\", \
                s.stage, s.stage_at, s.stage_done, s.stage_total, s.stage_detail, \
                min(t.created_at) FILTER (WHERE t.state = 'queued') AS waiting_since \
         FROM scan_runs r \
         LEFT JOIN scan_tasks t ON t.run_id = r.id \
         LEFT JOIN LATERAL ( \
             SELECT h.stage, h.stage_at, h.stage_done, h.stage_total, h.stage_detail \
             FROM scan_tasks h \
             WHERE h.run_id = r.id AND h.state = 'claimed' \
             ORDER BY h.claimed_at \
             LIMIT 1 \
         ) s ON true \
         WHERE r.state IN ('queued','running') \
         GROUP BY r.id, s.stage, s.stage_at, s.stage_done, s.stage_total, s.stage_detail",
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

/// One settled task, as the live tail reports it.
#[derive(Debug, Clone)]
pub struct TaskEvent {
    /// The task.
    pub id: Uuid,
    /// The run it belonged to.
    pub run_id: Uuid,
    /// Provider of the owning run, `None` once that provider is deleted.
    pub provider_slug: Option<String>,
    /// What the task was asked to do.
    pub kind: String,
    /// How it settled: `done`, `failed` or `skipped`.
    pub state: TaskState,
    /// What the task was pointed at — the page or series path, as the planner wrote it.
    pub target: Json,
    /// Why it failed, `None` unless it did.
    pub error: Option<String>,
    /// Claims taken on it before it settled.
    pub attempts: i16,
    /// When it settled, which is what the tail is ordered by.
    pub finished_at: Option<OffsetDateTime>,
}

/// The most recently settled tasks belonging to runs still in flight, newest first.
///
/// Scoped to in-flight runs on purpose: this is the "what is happening right now" tail, so it
/// must go quiet when nothing is running rather than replaying the last scan forever. It is also
/// what keeps the statement cheap — the alternative, ordering every task ever settled by
/// `finished_at`, would need an index on a column written once per task per scan.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; an idle deployment is an empty `Vec`.
pub async fn recent_task_activity<'e, E: PgExecutor<'e>>(
    exec: E,
    limit: i64,
) -> DbResult<Vec<TaskEvent>> {
    let rows = sqlx::query_as!(
        TaskEvent,
        "SELECT t.id, t.run_id, p.slug AS \"provider_slug?\", t.kind, \
                t.state AS \"state: TaskState\", t.target AS \"target: Json\", \
                t.error, t.attempts, t.finished_at \
         FROM scan_tasks t \
         JOIN scan_runs r ON r.id = t.run_id \
         LEFT JOIN providers p ON p.id = r.provider_id \
         WHERE r.state IN ('queued','running') AND t.finished_at IS NOT NULL \
         ORDER BY t.finished_at DESC \
         LIMIT $1",
        limit,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}
