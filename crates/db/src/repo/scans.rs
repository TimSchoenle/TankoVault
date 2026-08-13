//! Scan run + task repository. The task row is the truth for progress/audit; `JetStream` is
//! the truth for dispatch, with a `FOR UPDATE SKIP LOCKED` claim path as fallback.
//!
//! Settle-once: every claim/settle statement excludes the same three terminal states, so a
//! task counts toward `done_tasks`/`failed_tasks` at most once (`tests/repo_scans.rs` pins this).

use crate::error::{DbError, DbResult};
use serde_json::Value as Json;
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::{
    ProviderId, RunState, ScanMode, ScanRun, ScanRunId, ScanStage, ScanTask, ScanTaskId,
    StageTimings, TaskState,
};
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(FromRow)]
struct RunRow {
    id: Uuid,
    provider_id: Option<Uuid>,
    mode: ScanMode,
    state: RunState,
    total_tasks: i32,
    done_tasks: i32,
    failed_tasks: i32,
    started_at: Option<OffsetDateTime>,
    finished_at: Option<OffsetDateTime>,
    created_at: OffsetDateTime,
}

impl From<RunRow> for ScanRun {
    fn from(r: RunRow) -> Self {
        Self {
            id: ScanRunId::from_uuid(r.id),
            provider_id: r.provider_id.map(ProviderId::from_uuid),
            mode: r.mode,
            state: r.state,
            total_tasks: r.total_tasks,
            done_tasks: r.done_tasks,
            failed_tasks: r.failed_tasks,
            started_at: r.started_at,
            finished_at: r.finished_at,
            created_at: r.created_at,
        }
    }
}

/// Create a queued scan run.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. A `provider_id` that does not
/// exist is a foreign-key violation and so a 500, not [`crate::DbError::NotFound`]; `None` is a
/// legitimate value meaning "all providers" rather than a missing one.
pub async fn create_run<'e, E: PgExecutor<'e>>(
    exec: E,
    provider_id: Option<ProviderId>,
    mode: ScanMode,
) -> DbResult<ScanRunId> {
    let id = ScanRunId::new();
    sqlx::query!(
        "INSERT INTO scan_runs (id, provider_id, mode) VALUES ($1,$2,$3)",
        id.as_uuid(),
        provider_id.map(ProviderId::as_uuid),
        mode as ScanMode,
    )
    .execute(exec)
    .await?;
    Ok(id)
}

/// Fetch a run by id.
///
/// # Errors
/// - [`crate::DbError::NotFound`] if no run carries this id — one of the few functions here that
///   raises it rather than answering `Ok(None)`, because every caller is serving a "show me this
///   run" request where the miss *is* the 404.
/// - [`crate::DbError::Sqlx`] for any driver or connection failure.
pub async fn get_run<'e, E: PgExecutor<'e>>(exec: E, id: ScanRunId) -> DbResult<ScanRun> {
    let row = sqlx::query_as!(
        RunRow,
        "SELECT id, provider_id, mode AS \"mode: ScanMode\", state AS \"state: RunState\", \
         total_tasks, done_tasks, failed_tasks, started_at, finished_at, created_at \
         FROM scan_runs WHERE id = $1",
        id.as_uuid(),
    )
    .fetch_optional(exec)
    .await?;
    Ok(row.ok_or(DbError::NotFound)?.into())
}

/// A scan run with the slug of the provider it was scoped to.
///
/// The slug is not decoration. The console labels and narrows runs by it, and a bare
/// `provider_id` renders as an opaque uuid an operator cannot match to anything they typed — so
/// a pushed list carrying only the id cannot be filtered client-side at all, which is what made
/// the panel's provider filter look broken.
#[derive(Debug, Clone)]
pub struct RunListing {
    pub run: ScanRun,
    /// `None` for an all-provider run, and for a run whose provider has since been deleted.
    pub provider_slug: Option<String>,
}

#[derive(FromRow)]
struct ListingRow {
    id: Uuid,
    provider_id: Option<Uuid>,
    provider_slug: Option<String>,
    mode: ScanMode,
    state: RunState,
    total_tasks: i32,
    done_tasks: i32,
    failed_tasks: i32,
    started_at: Option<OffsetDateTime>,
    finished_at: Option<OffsetDateTime>,
    created_at: OffsetDateTime,
}

impl From<ListingRow> for RunListing {
    fn from(r: ListingRow) -> Self {
        Self {
            provider_slug: r.provider_slug,
            run: ScanRun::from(RunRow {
                id: r.id,
                provider_id: r.provider_id,
                mode: r.mode,
                state: r.state,
                total_tasks: r.total_tasks,
                done_tasks: r.done_tasks,
                failed_tasks: r.failed_tasks,
                started_at: r.started_at,
                finished_at: r.finished_at,
                created_at: r.created_at,
            }),
        }
    }
}

/// List recent runs (console overview and the live push).
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. A deployment that has never
/// scanned is an empty `Vec`, not [`crate::DbError::NotFound`].
pub async fn list_recent_runs<'e, E: PgExecutor<'e>>(
    exec: E,
    limit: i64,
) -> DbResult<Vec<RunListing>> {
    let rows = sqlx::query_as!(
        ListingRow,
        "SELECT r.id, r.provider_id, p.slug AS \"provider_slug?\", \
                r.mode AS \"mode: ScanMode\", r.state AS \"state: RunState\", \
                r.total_tasks, r.done_tasks, r.failed_tasks, \
                r.started_at, r.finished_at, r.created_at \
         FROM scan_runs r \
         LEFT JOIN providers p ON p.id = r.provider_id \
         ORDER BY r.created_at DESC LIMIT $1",
        limit,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows.into_iter().map(RunListing::from).collect())
}

/// How a page of runs is ordered.
///
/// A *server-side* choice rather than a sort of the page already fetched: the history is capped
/// at 200 rows and "the runs that failed most" is a question about the whole window, not about
/// whichever 30 rows arrived first.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RunSort {
    /// Newest first.
    #[default]
    Recent,
    Oldest,
    /// Most failed tasks first.
    Failures,
    /// Longest wall-clock first; a run still in flight is measured against `now()`.
    Duration,
}

impl RunSort {
    /// The token this ordering is selected by, and what the statement below compares against.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Recent => "recent",
            Self::Oldest => "oldest",
            Self::Failures => "failures",
            Self::Duration => "duration",
        }
    }
}

/// What narrows a page of scan runs.
#[derive(Debug, Clone, Default)]
pub struct RunFilter<'a> {
    /// Provider slug, not id: the console filters from the URL, and a slug is what an operator
    /// can type and paste.
    pub provider: Option<&'a str>,
    pub mode: Option<ScanMode>,
    pub state: Option<RunState>,
    pub since: Option<OffsetDateTime>,
    pub sort: RunSort,
}

/// One page of scan runs, plus how many the filter matches in total.
#[derive(Debug, Clone)]
pub struct RunPage {
    pub items: Vec<RunListing>,
    pub total: i64,
}

/// A filtered, paged, ordered window on the run history.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only. A filter matching nothing is an empty page with `total: 0`;
/// as elsewhere, `total` is read off the first row, so an offset past the end reports `0`.
pub async fn list_runs_filtered<'e, E: PgExecutor<'e>>(
    exec: E,
    filter: &RunFilter<'_>,
    limit: i64,
    offset: i64,
) -> DbResult<RunPage> {
    struct Row {
        id: Uuid,
        provider_id: Option<Uuid>,
        provider_slug: Option<String>,
        mode: ScanMode,
        state: RunState,
        total_tasks: i32,
        done_tasks: i32,
        failed_tasks: i32,
        started_at: Option<OffsetDateTime>,
        finished_at: Option<OffsetDateTime>,
        created_at: OffsetDateTime,
        total: i64,
    }
    // Every ordering branch collapses to NULL unless its token is the one asked for, so the
    // trailing `created_at DESC` is both the default and the tie-break — one statement rather
    // than four near-identical ones the offline cache would have to carry separately.
    let rows = sqlx::query_as!(
        Row,
        "WITH matched AS ( \
             SELECT r.*, p.slug AS provider_slug FROM scan_runs r \
             LEFT JOIN providers p ON p.id = r.provider_id \
             WHERE ($1::text IS NULL OR p.slug = $1) \
               AND ($2::scan_mode IS NULL OR r.mode = $2) \
               AND ($3::run_state IS NULL OR r.state = $3) \
               AND ($4::timestamptz IS NULL OR r.created_at >= $4) \
         ) \
         SELECT m.id, m.provider_id, m.provider_slug AS \"provider_slug?\", \
                m.mode AS \"mode: ScanMode\", \
                m.state AS \"state: RunState\", m.total_tasks, m.done_tasks, m.failed_tasks, \
                m.started_at, m.finished_at, m.created_at, \
                (SELECT count(*) FROM matched) AS \"total!\" \
         FROM matched m \
         ORDER BY \
           CASE WHEN $5::text = 'failures' THEN m.failed_tasks END DESC NULLS LAST, \
           CASE WHEN $5::text = 'duration' \
                THEN EXTRACT(EPOCH FROM (COALESCE(m.finished_at, now()) - m.started_at)) \
           END DESC NULLS LAST, \
           CASE WHEN $5::text = 'oldest' THEN m.created_at END ASC, \
           m.created_at DESC \
         LIMIT $6 OFFSET $7",
        filter.provider,
        filter.mode as Option<ScanMode>,
        filter.state as Option<RunState>,
        filter.since,
        filter.sort.token(),
        limit,
        offset,
    )
    .fetch_all(exec)
    .await?;

    let total = rows.first().map_or(0, |row| row.total);
    let items = rows
        .into_iter()
        .map(|r| {
            RunListing::from(ListingRow {
                id: r.id,
                provider_id: r.provider_id,
                provider_slug: r.provider_slug,
                mode: r.mode,
                state: r.state,
                total_tasks: r.total_tasks,
                done_tasks: r.done_tasks,
                failed_tasks: r.failed_tasks,
                started_at: r.started_at,
                finished_at: r.finished_at,
                created_at: r.created_at,
            })
        })
        .collect();
    Ok(RunPage { items, total })
}

/// How many runs of one provider and mode have failed in a row, and when the last of them
/// finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FailureStreak {
    /// Consecutive failed runs, most recent first, stopping at the first that was not failed.
    /// Saturates at the window [`failure_streak`] reads.
    pub failures: i64,
    /// When the most recent of them finished. `None` exactly when `failures` is 0.
    pub last_failed_at: Option<OffsetDateTime>,
}

/// How many finished runs are examined for a streak.
///
/// The streak only ever drives a capped backoff, so a longer window buys nothing: any count past
/// the cap produces the same wait. Bounding it is what keeps the read a short index scan on a
/// table that grows by a run per provider per sweep.
const FAILURE_STREAK_WINDOW: i64 = 32;

/// The provider's current run of consecutive failures in `mode`.
///
/// Derived from `scan_runs` rather than tracked in a column of its own, deliberately: the runs
/// *are* the record of what happened, and a second copy of it would be one more thing to keep
/// true. A provider that has never finished a run of this mode has no streak.
///
/// Only finished runs count. One in flight is not evidence of anything yet, and letting it end
/// the streak would clear the backoff every time a run was queued — which is every sweep.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only. A provider with no history is a zero streak, not
/// [`crate::DbError::NotFound`]: "nothing has failed" is the ordinary answer.
pub async fn failure_streak<'e, E: PgExecutor<'e>>(
    exec: E,
    provider_id: ProviderId,
    mode: ScanMode,
) -> DbResult<FailureStreak> {
    // `rn` numbers the finished runs newest-first; the streak is every row ahead of the first
    // one that did not fail. With no such row (everything in the window failed) the bound is the
    // window itself, so the count saturates instead of ending.
    let row = sqlx::query!(
        "WITH recent AS ( \
             SELECT state, finished_at, \
                    row_number() OVER (ORDER BY created_at DESC) AS rn \
             FROM scan_runs \
             WHERE provider_id = $1 AND mode = $2::scan_mode \
               AND state IN ('completed','failed') \
             ORDER BY created_at DESC \
             LIMIT $3 \
         ) \
         SELECT count(*) AS \"failures!\", max(finished_at) AS \"last_failed_at?\" \
         FROM recent \
         WHERE rn < COALESCE((SELECT min(rn) FROM recent WHERE state <> 'failed'), $3 + 1)",
        provider_id.as_uuid(),
        mode as ScanMode,
        FAILURE_STREAK_WINDOW,
    )
    .fetch_one(exec)
    .await?;

    Ok(FailureStreak {
        failures: row.failures,
        last_failed_at: row.last_failed_at,
    })
}

/// The newest run of `mode` this provider still has in flight, if any — the planner's guard
/// against queueing a second one behind it.
///
/// "In flight" is narrower than the run state alone, because two states look identical in
/// `scan_runs` and must not be treated alike:
///
/// - A run whose tasks have all settled but which was never finalised — the terminal progress
///   event is best-effort, and a lost one leaves the row `running` with nothing left to do.
///   Such a run is **not** in flight; the `EXISTS` excludes it. `total_tasks = 0` is the
///   converse: a run whose planner has not created its first task yet, which *is* in flight.
/// - A run whose task was persisted but never published (the planner died between the two),
///   so nothing will ever settle it. Only age separates that from a slow scan, which is what
///   `stale_after` is for: past it a run stops suppressing new ones, or one lost publish would
///   retire the provider from scanning permanently.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only. No in-flight run is `Ok(None)`, never
/// [`crate::DbError::NotFound`] — the absence is the ordinary answer and the caller's cue to
/// plan.
pub async fn in_flight_run<'e, E: PgExecutor<'e>>(
    exec: E,
    provider_id: ProviderId,
    mode: ScanMode,
    stale_after: std::time::Duration,
) -> DbResult<Option<ScanRunId>> {
    let row = sqlx::query_scalar!(
        "SELECT r.id FROM scan_runs r \
         WHERE r.provider_id = $1 \
           AND r.mode = $2::scan_mode \
           AND r.state IN ('queued','running') \
           AND r.created_at > now() - make_interval(secs => $3) \
           AND (r.total_tasks = 0 OR EXISTS ( \
                   SELECT 1 FROM scan_tasks t \
                   WHERE t.run_id = r.id AND t.state IN ('queued','claimed','running') \
               )) \
         ORDER BY r.created_at DESC \
         LIMIT 1",
        provider_id.as_uuid(),
        mode as ScanMode,
        stale_after.as_secs_f64(),
    )
    .fetch_optional(exec)
    .await?;
    Ok(row.map(ScanRunId::from_uuid))
}

/// Transition a run to `running` and stamp `started_at`.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. An unknown `id` matches nothing
/// and is still `Ok(())`, not [`crate::DbError::NotFound`]. There is no state guard either: this
/// will move a `completed` run back to `running`, which is safe only because the planner is the
/// sole caller and calls it once, before any task settles. The `COALESCE` protects the one thing
/// a repeat would otherwise corrupt — `started_at` keeps the first start, so a run's duration
/// cannot be reset by a redelivered plan.
pub async fn start_run<'e, E: PgExecutor<'e>>(exec: E, id: ScanRunId) -> DbResult<()> {
    sqlx::query!(
        "UPDATE scan_runs SET state = 'running', started_at = COALESCE(started_at, now()) \
         WHERE id = $1",
        id.as_uuid(),
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Set a run's final state and stamp `finished_at`.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. An unknown `id` is `Ok(())`
/// with nothing written. This is the **unconditional** counterpart to
/// [`finalize_if_complete`]: it has no `state = 'running'` guard, so it will re-stamp an
/// already-finished run and cannot tell a caller whether it performed the transition. Use it for
/// the operator-forced and plan-failed paths; use `finalize_if_complete` wherever exactly one
/// terminal event must be emitted.
pub async fn finish_run<'e, E: PgExecutor<'e>>(
    exec: E,
    id: ScanRunId,
    state: RunState,
) -> DbResult<()> {
    sqlx::query!(
        "UPDATE scan_runs SET state = $2, finished_at = now() WHERE id = $1",
        id.as_uuid(),
        state as RunState,
    )
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
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. `Ok(None)` carries four
/// distinct situations deliberately collapsed into one, because the caller's response to all of
/// them is the same — emit nothing: the run is not yet complete, another actor finalised it
/// first, `total_tasks` is still `0` (a plan that has not fanned out yet), or the id names no
/// run at all. It is never [`crate::DbError::NotFound`]; a missing run must not fail the
/// aggregator, which reaches this on every task settle.
///
/// Do not default a failure to `Ok(None)`: the terminal event is emitted exactly once, so
/// swallowing the error leaves a run that has genuinely completed sitting in `running` forever
/// with nothing to retry it.
pub async fn finalize_if_complete<'e, E: PgExecutor<'e>>(
    exec: E,
    id: ScanRunId,
) -> DbResult<Option<ScanRun>> {
    let row = sqlx::query_as!(
        RunRow,
        "UPDATE scan_runs SET \
            state = CASE WHEN done_tasks = 0 AND failed_tasks > 0 \
                         THEN 'failed'::run_state ELSE 'completed'::run_state END, \
            finished_at = now() \
         WHERE id = $1 AND state = 'running' AND total_tasks > 0 \
               AND (done_tasks + failed_tasks) >= total_tasks \
         RETURNING id, provider_id, mode AS \"mode: ScanMode\", state AS \"state: RunState\", \
                   total_tasks, done_tasks, failed_tasks, started_at, finished_at, created_at",
        id.as_uuid(),
    )
    .fetch_optional(exec)
    .await?;
    Ok(row.map(ScanRun::from))
}

/// Add to a run's planned task total (as the planner fans out).
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. An unknown `id` is `Ok(())`
/// with nothing written, not [`crate::DbError::NotFound`]. The arithmetic is plain `+`, so a
/// `delta` large enough to overflow `int4` is a driver error rather than a wrapped total —
/// which is the answer that matters, since a wrapped `total_tasks` would make
/// [`finalize_if_complete`]'s `>=` fire immediately and end the run before its tasks ran.
///
/// This must not be swallowed: the total is one half of the comparison that decides a run is
/// over, so a lost increment finalises the run early and the tasks it forgot settle into a run
/// that has already emitted its terminal event.
pub async fn add_total_tasks<'e, E: PgExecutor<'e>>(
    exec: E,
    id: ScanRunId,
    delta: i32,
) -> DbResult<()> {
    sqlx::query!(
        "UPDATE scan_runs SET total_tasks = total_tasks + $2 WHERE id = $1",
        id.as_uuid(),
        delta,
    )
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
    state: TaskState,
    attempts: i16,
    worker_id: Option<String>,
    error: Option<String>,
    claimed_at: Option<OffsetDateTime>,
    finished_at: Option<OffsetDateTime>,
}

impl From<TaskRow> for ScanTask {
    fn from(r: TaskRow) -> Self {
        Self {
            id: ScanTaskId::from_uuid(r.id),
            run_id: ScanRunId::from_uuid(r.run_id),
            kind: r.kind,
            target: r.target,
            state: r.state,
            attempts: r.attempts,
            worker_id: r.worker_id,
            error: r.error,
            claimed_at: r.claimed_at,
            finished_at: r.finished_at,
        }
    }
}

/// Create a queued task and return its id, or `None` when an identical task
/// (`run_id`, `kind`, `target`) already exists.
///
/// The `ON CONFLICT DO NOTHING` makes fan-out idempotent under at-least-once delivery: a
/// redelivered `catalog_page` re-attempts the same child inserts and simply gets `None`
/// back, so the caller skips the duplicate `add_total_tasks` + publish instead of
/// re-enqueuing every series (design §12). Relies on the `scan_tasks_run_kind_target`
/// unique index.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable; an unknown `run_id` is a
/// foreign-key violation. `Ok(None)` means "already present", never
/// [`crate::DbError::Conflict`] — the duplicate is the expected outcome of a redelivery rather
/// than a client error, and the caller's correct response is to skip the publish, not to
/// surface a 409.
pub async fn create_task<'e, E: PgExecutor<'e>>(
    exec: E,
    run_id: ScanRunId,
    kind: &str,
    target: &Json,
) -> DbResult<Option<ScanTaskId>> {
    let id = ScanTaskId::new();
    let inserted = sqlx::query_scalar!(
        "INSERT INTO scan_tasks (id, run_id, kind, target) VALUES ($1,$2,$3,$4) \
         ON CONFLICT (run_id, kind, (target::text)) DO NOTHING RETURNING id",
        id.as_uuid(),
        run_id.as_uuid(),
        kind,
        target,
    )
    .fetch_optional(exec)
    .await?;
    Ok(inserted.map(ScanTaskId::from_uuid))
}

/// Create many queued tasks of the same `kind` in one statement, returning the
/// `(id, target)` pairs that were **actually inserted**.
///
/// Semantically identical to calling [`create_task`] per target — including the
/// `ON CONFLICT DO NOTHING` idempotency — but collapses N round-trips into one. Targets
/// already present (a redelivered parent re-attempting its fan-out) are simply absent from
/// the result, so the caller publishes only genuinely new tasks. Duplicates *within* one
/// batch are also resolved by the conflict clause, which `DO NOTHING` handles safely.
///
/// Callers are expected to chunk large fan-outs; see `CATALOG_FANOUT_CHUNK` in the worker.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable, with the same foreign-key
/// shape as [`create_task`]. An empty `targets` returns an empty `Vec` without issuing a
/// statement.
///
/// The empty `Vec` is doing double duty and must not be read as failure: it is also what a fully
/// redelivered fan-out returns — every target already present, nothing to publish — which is the
/// ordinary result of a retry. Only the `Err` says nothing was written.
pub async fn create_tasks<'e, E: PgExecutor<'e>>(
    exec: E,
    run_id: ScanRunId,
    kind: &str,
    targets: &[Json],
) -> DbResult<Vec<(ScanTaskId, Json)>> {
    if targets.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<Uuid> = targets
        .iter()
        .map(|_| ScanTaskId::new().as_uuid())
        .collect();
    let rows = sqlx::query!(
        "INSERT INTO scan_tasks (id, run_id, kind, target) \
         SELECT * FROM UNNEST($1::uuid[], $2::uuid[], $3::text[], $4::jsonb[]) \
         ON CONFLICT (run_id, kind, (target::text)) DO NOTHING \
         RETURNING id, target",
        &ids,
        &vec![run_id.as_uuid(); targets.len()],
        &vec![kind.to_owned(); targets.len()],
        targets,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (ScanTaskId::from_uuid(r.id), r.target))
        .collect())
}

/// Mark a task claimed by a worker, returning whether the work should actually be done.
///
/// A task that has already **settled** is not re-claimed. That guard is what makes the run
/// counters honest under at-least-once delivery: without it a redelivery of a task the worker
/// already completed put the row back into `claimed`, which re-opened
/// [`complete_task`]'s `state <> 'done'` guard and incremented `done_tasks` a second time for
/// one task. `finalize_if_complete` fires on `done_tasks + failed_tasks >= total_tasks`, so two
/// counts for one task finalise the run — and emit its single terminal event — while other
/// tasks are still running.
///
/// A task whose **run has been cancelled** is not claimed either, and this is the one thing the
/// caller must act on. `JetStream` holds the message independently of the database, so cancelling
/// a run cannot unpublish its tasks; the only place the cancellation can be honoured is here,
/// immediately before the work would start. `false` therefore means "settle this message and do
/// nothing" — a worker that ignored it would keep crawling a provider an operator has told it to
/// stop crawling.
///
/// Claiming also opens the task's stage at [`tankovault_domain::ScanStage::Starting`] and clears
/// any stage a previous delivery left behind, so a redelivered task never shows the progress of
/// its abandoned attempt.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. `Ok(false)` collapses three
/// situations the caller responds to identically by not doing the work: an unknown task, a task
/// that has already settled, and a task whose run was cancelled. It is never
/// [`crate::DbError::NotFound`]. `crates/db/tests/repo_scans.rs` pins the counter behaviour that
/// depends on the settle-once half.
pub async fn claim_task<'e, E: PgExecutor<'e>>(
    exec: E,
    task_id: ScanTaskId,
    worker_id: &str,
) -> DbResult<bool> {
    let claimed = sqlx::query_scalar!(
        "UPDATE scan_tasks t SET state = 'claimed', worker_id = $2, claimed_at = now(), \
             attempts = t.attempts + 1, \
             stage = 'starting', stage_at = now(), \
             stage_done = NULL, stage_total = NULL, stage_detail = NULL \
         FROM scan_runs r \
         WHERE t.id = $1 AND r.id = t.run_id \
           AND t.state NOT IN ('done','failed','skipped') \
           AND r.state <> 'cancelled' \
         RETURNING t.id",
        task_id.as_uuid(),
        worker_id,
    )
    .fetch_optional(exec)
    .await?;
    Ok(claimed.is_some())
}

/// Durable fallback claim: atomically grab the oldest queued task for a run using
/// `FOR UPDATE SKIP LOCKED`, used when the broker is unavailable.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. `Ok(None)` means the run has
/// no claimable task *right now* — nothing queued, or every queued row locked by another worker,
/// which `SKIP LOCKED` makes indistinguishable on purpose — and never
/// [`crate::DbError::NotFound`], so an unknown `run_id` looks the same as a drained one. The
/// caller polls; it must not read `None` as "the run is finished", since a task another worker
/// is about to release is also `None`.
pub async fn claim_next_queued<'e, E: PgExecutor<'e>>(
    exec: E,
    run_id: ScanRunId,
    worker_id: &str,
) -> DbResult<Option<ScanTask>> {
    let row = sqlx::query_as!(
        TaskRow,
        "UPDATE scan_tasks SET state = 'claimed', worker_id = $2, claimed_at = now(), \
             attempts = attempts + 1 \
         WHERE id = ( \
            SELECT id FROM scan_tasks WHERE run_id = $1 AND state = 'queued' \
            ORDER BY id FOR UPDATE SKIP LOCKED LIMIT 1 \
         ) \
         RETURNING id, run_id, kind, target AS \"target: Json\", state AS \"state: TaskState\", \
                   attempts, worker_id, error, claimed_at, finished_at",
        run_id.as_uuid(),
        worker_id,
    )
    .fetch_optional(exec)
    .await?;
    Ok(row.map(ScanTask::from))
}

/// Record which stage a claimed task is in, how far through it is, and against what.
///
/// `stage_at` only moves when the stage itself changes, so "in this stage since" stays stable
/// while a stage reports progress — a timer that reset on every counter tick would never show the
/// one thing it exists for, a stage that has stopped advancing.
///
/// Guarded on `state = 'claimed'`: a stage write that lost a race with the task settling would
/// otherwise reopen a live stage on a finished row, and the console would show a completed run
/// still fetching.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only. An unknown or already-settled task is `Ok(())` with nothing
/// written — this is diagnostic, and a caller must never fail a scan because a progress update
/// did not land.
pub async fn set_task_stage<'e, E: PgExecutor<'e>>(
    exec: E,
    task_id: ScanTaskId,
    stage: ScanStage,
    progress: Option<(i32, i32)>,
    detail: Option<&str>,
) -> DbResult<()> {
    sqlx::query!(
        "UPDATE scan_tasks SET \
             stage = $2, \
             stage_at = CASE WHEN stage IS DISTINCT FROM $2 THEN now() ELSE stage_at END, \
             stage_done = $3, stage_total = $4, stage_detail = $5 \
         WHERE id = $1 AND state = 'claimed'",
        task_id.as_uuid(),
        stage.as_str(),
        progress.map(|(done, _)| done),
        progress.map(|(_, total)| total),
        detail,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// What a settled task reports about how it spent its time.
///
/// Optional at every settle site: the durable claim path and the tests settle tasks they never
/// instrumented, and a scan must not depend on its own telemetry being present.
#[derive(Debug, Clone, Copy)]
pub struct TaskOutcome<'a> {
    /// Wall clock from claim to settle. Saturating at `i32::MAX` ms (~24 days) is the caller's
    /// job; the column is `int`.
    pub duration_ms: i32,
    pub timings: &'a StageTimings,
}

impl TaskOutcome<'_> {
    /// The telemetry blob as the column stores it, or `None` when it cannot be encoded — which
    /// this type's shape makes unreachable, and which must never fail a settle if it happens.
    fn payload(this: Option<&Self>) -> Option<Json> {
        this.and_then(|outcome| serde_json::to_value(outcome.timings).ok())
    }
}

/// Mark a task done and increment the run's done counter, atomically per statement, recording
/// what the work cost.
///
/// The guard excludes **every** terminal state, not just `done`: a task is counted once, on the
/// first settle that reaches it. Guarding only `state <> 'done'` let a redelivery that failed
/// after an earlier success add a `failed_tasks` count on top of the `done_tasks` one, taking
/// `done_tasks + failed_tasks` above `total_tasks` — see [`claim_task`].
///
/// `wait_ms` keeps the **first** value it was given, so a retried task reports how long it
/// originally waited for a worker rather than the near-zero wait of its redelivery.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. An unknown task, and a task
/// that has already settled, are both `Ok(())` with neither the row nor the counter touched —
/// that silence *is* the settle-once property, not an oversight, and it is why a redelivery is
/// safe to run to completion.
///
/// This is the error most worth propagating in the module: the task row and the run counter move
/// in one statement, so a failure leaves the task unsettled and the run one count short of
/// finalising. `JetStream` redelivery is what recovers it, which only happens if the worker
/// declines to ack — so swallowing this error strands the run.
pub async fn complete_task<'e, E: PgExecutor<'e>>(
    exec: E,
    task_id: ScanTaskId,
    outcome: Option<&TaskOutcome<'_>>,
) -> DbResult<()> {
    // Two statements would race on the counter under concurrency; a CTE keeps them in
    // one round trip and lets each worker's increment commit independently.
    sqlx::query!(
        "WITH done AS ( \
            UPDATE scan_tasks SET state = 'done', finished_at = now(), \
                duration_ms = COALESCE($2, duration_ms), \
                telemetry = COALESCE($3, telemetry), \
                wait_ms = COALESCE(wait_ms, LEAST( \
                    EXTRACT(EPOCH FROM (COALESCE(claimed_at, now()) - created_at)) * 1000, \
                    2147483647)::int) \
            WHERE id = $1 AND state NOT IN ('done','failed','skipped') RETURNING run_id \
         ) \
         UPDATE scan_runs SET done_tasks = done_tasks + 1 \
         WHERE id = (SELECT run_id FROM done)",
        task_id.as_uuid(),
        outcome.map(|o| o.duration_ms),
        TaskOutcome::payload(outcome),
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Mark a task failed with an error, incrementing the run's failed counter and recording what the
/// attempt cost before it failed.
///
/// Same settle-once guard as [`complete_task`]: an already-settled task keeps the state and the
/// count it first reached, so a redelivery cannot turn a completed task into a failed one and
/// have the run count it twice. The stage the row is carrying is deliberately left alone — the
/// stage a task died in is the most useful single field on a failure.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable, with the same silent-no-op and
/// stranded-run notes as [`complete_task`]. One difference is worth stating: this runs on a path
/// that is *already* handling a failure, so a caller that logs rather than propagates loses the
/// record of why the task failed **and** the count that ends the run. Both halves are in this one
/// statement precisely so neither can be lost without the other.
pub async fn fail_task<'e, E: PgExecutor<'e>>(
    exec: E,
    task_id: ScanTaskId,
    error: &str,
    outcome: Option<&TaskOutcome<'_>>,
) -> DbResult<()> {
    sqlx::query!(
        "WITH failed AS ( \
            UPDATE scan_tasks SET state = 'failed', error = $2, finished_at = now(), \
                duration_ms = COALESCE($3, duration_ms), \
                telemetry = COALESCE($4, telemetry), \
                wait_ms = COALESCE(wait_ms, LEAST( \
                    EXTRACT(EPOCH FROM (COALESCE(claimed_at, now()) - created_at)) * 1000, \
                    2147483647)::int) \
            WHERE id = $1 AND state NOT IN ('done','failed','skipped') RETURNING run_id \
         ) \
         UPDATE scan_runs SET failed_tasks = failed_tasks + 1 \
         WHERE id = (SELECT run_id FROM failed)",
        task_id.as_uuid(),
        error,
        outcome.map(|o| o.duration_ms),
        TaskOutcome::payload(outcome),
    )
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
    /// When an operator cleared this failure from the triage feed, if they have.
    #[serde(with = "time::serde::rfc3339::option")]
    pub acknowledged_at: Option<OffsetDateTime>,
}

/// Recent failed tasks, narrowed by provider and time window — the operator's triage feed for
/// stuck providers and broken selectors (design §17.2.7).
///
/// `include_cleared` reopens the failures an operator has acknowledged. They are excluded by
/// default, which is the whole point of clearing; they are never deleted, so the window can
/// always be re-read in full.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; no failures is an empty `Vec`, which is the feed's goal state.
/// The join to `providers` is a `LEFT` join on purpose — a run whose provider has since been
/// deleted still appears, with `provider_slug: None`, rather than dropping out of the triage feed
/// exactly when an operator deleted the thing that was failing.
pub async fn failed_tasks_filtered<'e, E: PgExecutor<'e>>(
    exec: E,
    provider: Option<&str>,
    since: Option<OffsetDateTime>,
    include_cleared: bool,
    limit: i64,
) -> DbResult<Vec<FailedTaskView>> {
    let rows = sqlx::query_as!(
        FailedTaskView,
        "SELECT t.id, t.run_id, p.slug AS \"provider_slug?\", r.mode::text AS \"mode!\", \
                t.kind, t.error, t.attempts, t.finished_at, t.acknowledged_at \
         FROM scan_tasks t \
         JOIN scan_runs r ON r.id = t.run_id \
         LEFT JOIN providers p ON p.id = r.provider_id \
         WHERE t.state = 'failed' \
           AND ($1::text IS NULL OR p.slug = $1) \
           AND ($2::timestamptz IS NULL OR t.finished_at >= $2) \
           AND ($3::bool OR t.acknowledged_at IS NULL) \
         ORDER BY t.finished_at DESC NULLS LAST \
         LIMIT $4",
        provider,
        since,
        include_cleared,
        limit,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

/// One distinct failure, with how often it happened and which providers it hit.
#[derive(Debug, Clone, FromRow)]
pub struct FailureGroup {
    /// The error text these failures share. `None` groups the failures that recorded none.
    pub error: Option<String>,
    pub count: i64,
    /// How many of `count` an operator has already cleared. Non-zero only when the caller asked
    /// for cleared failures back.
    pub cleared: i64,
    /// Provider slugs affected, sorted. A provider deleted since is omitted, not `null`.
    pub providers: Vec<String>,
    /// Task kinds this error struck, sorted — a `series` failure and a `catalog_page` failure
    /// with the same message are different problems.
    pub kinds: Vec<String>,
    pub latest_at: Option<OffsetDateTime>,
}

/// Failed tasks collapsed by their error text, worst first.
///
/// Twelve rows of one broken selector is one problem; the flat feed presents it as twelve, and
/// on a bad day it is the whole feed. Grouping is what makes the panel answer "what is wrong"
/// rather than "what happened most recently".
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; no failures is an empty `Vec`.
pub async fn failure_groups<'e, E: PgExecutor<'e>>(
    exec: E,
    provider: Option<&str>,
    since: Option<OffsetDateTime>,
    include_cleared: bool,
    limit: i64,
) -> DbResult<Vec<FailureGroup>> {
    let rows = sqlx::query_as!(
        FailureGroup,
        "SELECT t.error, \
                count(*) AS \"count!\", \
                count(*) FILTER (WHERE t.acknowledged_at IS NOT NULL) AS \"cleared!\", \
                array_remove(array_agg(DISTINCT p.slug), NULL) AS \"providers!\", \
                array_agg(DISTINCT t.kind) AS \"kinds!\", \
                max(t.finished_at) AS latest_at \
         FROM scan_tasks t \
         JOIN scan_runs r ON r.id = t.run_id \
         LEFT JOIN providers p ON p.id = r.provider_id \
         WHERE t.state = 'failed' \
           AND ($1::text IS NULL OR p.slug = $1) \
           AND ($2::timestamptz IS NULL OR t.finished_at >= $2) \
           AND ($3::bool OR t.acknowledged_at IS NULL) \
         GROUP BY t.error \
         ORDER BY count(*) DESC, max(t.finished_at) DESC NULLS LAST \
         LIMIT $4",
        provider,
        since,
        include_cleared,
        limit,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

/// Which error group a clear selects.
///
/// Three-way on purpose. A plain `Option<&str>` cannot say "the failures that recorded no error
/// at all" — that reads identically to "any error", and conflating the two would turn a request
/// to clear one quiet group into a request to clear the whole feed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ErrorSelector<'a> {
    /// Every group.
    #[default]
    Any,
    /// The group whose failures recorded no error text.
    Absent,
    /// One named group, matched on the exact text the grouped feed reported.
    Exactly(&'a str),
}

impl<'a> ErrorSelector<'a> {
    /// Whether the statement should compare on the error at all.
    const fn is_narrowing(self) -> bool {
        !matches!(self, Self::Any)
    }

    /// The text to compare against; `None` both for [`Self::Any`], where the comparison is
    /// switched off, and for [`Self::Absent`], where `IS NOT DISTINCT FROM NULL` is the match.
    const fn text(self) -> Option<&'a str> {
        match self {
            Self::Exactly(text) => Some(text),
            Self::Any | Self::Absent => None,
        }
    }
}

/// What a clear request selects. Every field narrows; all of them at their default clears the
/// whole feed.
#[derive(Debug, Clone, Default)]
pub struct FailureSelector<'a> {
    pub provider: Option<&'a str>,
    pub since: Option<OffsetDateTime>,
    /// One run's failures.
    pub run_id: Option<ScanRunId>,
    pub error: ErrorSelector<'a>,
}

/// Clear the selected failures out of the triage feed.
///
/// Returns how many rows this call acknowledged. Already-cleared rows are excluded, so the count
/// is what *this* operator hid rather than what matches the selector — two operators clearing the
/// same feed do not both claim it.
///
/// This never deletes: the task keeps its `failed` state, its error and its counter contribution,
/// so a cleared feed still reconciles against the run history and the audit trail. An operator
/// dismissing noise must not be able to erase the record of an outage.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; a selector matching nothing is `Ok(0)`.
pub async fn clear_failures<'e, E: PgExecutor<'e>>(
    exec: E,
    selector: &FailureSelector<'_>,
) -> DbResult<u64> {
    // `IS NOT DISTINCT FROM` rather than `=`, because the group an operator is clearing may be
    // the one whose error is NULL, and `= NULL` matches nothing.
    let result = sqlx::query!(
        "UPDATE scan_tasks t SET acknowledged_at = now() \
         FROM scan_runs r LEFT JOIN providers p ON p.id = r.provider_id \
         WHERE r.id = t.run_id \
           AND t.state = 'failed' \
           AND t.acknowledged_at IS NULL \
           AND ($1::text IS NULL OR p.slug = $1) \
           AND ($2::timestamptz IS NULL OR t.finished_at >= $2) \
           AND ($3::uuid IS NULL OR t.run_id = $3) \
           AND (NOT $4::bool OR t.error IS NOT DISTINCT FROM $5::text)",
        selector.provider,
        selector.since,
        selector.run_id.map(ScanRunId::as_uuid),
        selector.error.is_narrowing(),
        selector.error.text(),
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected())
}

/// Mark a task skipped (unchanged content, no work needed) and count it as done.
///
/// Same settle-once guard as [`complete_task`]; excluding `failed` here too stops a failed task
/// from later being skipped and adding a `done_tasks` count next to its `failed_tasks` one.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable; the silent-no-op and
/// stranded-run notes on [`complete_task`] apply unchanged. Note that a skip increments
/// `done_tasks`, not a counter of its own: "nothing to do" settles a task as successfully as
/// doing the work, which is what lets an unchanged-content re-scan finalise its run at all.
pub async fn skip_task<'e, E: PgExecutor<'e>>(exec: E, task_id: ScanTaskId) -> DbResult<()> {
    sqlx::query!(
        "WITH skipped AS ( \
            UPDATE scan_tasks SET state = 'skipped', finished_at = now() \
            WHERE id = $1 AND state NOT IN ('done','failed','skipped') RETURNING run_id \
         ) \
         UPDATE scan_runs SET done_tasks = done_tasks + 1 \
         WHERE id = (SELECT run_id FROM skipped)",
        task_id.as_uuid(),
    )
    .execute(exec)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Cancellation
// ---------------------------------------------------------------------------

/// What a cancellation actually stopped.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Cancelled {
    /// Runs moved to `cancelled` by this call. Runs already terminal are not counted, so two
    /// operators cancelling the same queue do not both claim it.
    pub runs: i64,
    /// Tasks abandoned, across those runs.
    pub tasks: i64,
}

/// Cancel one in-flight run and abandon every task it still had outstanding.
///
/// Returns `None` when the run was already terminal or does not exist — cancelling a finished run
/// is a no-op, not an error, because two operators pressing the same button is ordinary.
///
/// ## What cancellation can and cannot reach
///
/// The run row and its task rows are ours; the queued **messages** are `JetStream`'s, and nothing
/// here can unpublish them. A worker that pulls one of those messages after this call finds the
/// task abandoned and its run cancelled, and [`claim_task`] answers `false` — that is where the
/// cancellation is actually honoured. So a cancelled provider stops being crawled at its next task
/// boundary, not mid-request: a task already in flight runs to its end, and its settle is a no-op
/// against the terminal counters.
///
/// Abandoned tasks are `skipped`, and — unlike [`skip_task`] — they do **not** bump `done_tasks`.
/// A skip means "nothing to do"; this means "told to stop", and counting it as done would render
/// a cancelled run as a completed one on the progress bar.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn cancel_run<'e, E: PgExecutor<'e>>(
    exec: E,
    run_id: ScanRunId,
) -> DbResult<Option<Cancelled>> {
    let row = sqlx::query!(
        "WITH stopped AS ( \
             UPDATE scan_runs SET state = 'cancelled', finished_at = now() \
             WHERE id = $1 AND state IN ('queued','running') \
             RETURNING id \
         ), abandoned AS ( \
             UPDATE scan_tasks SET state = 'skipped', finished_at = now() \
             WHERE run_id = (SELECT id FROM stopped) \
               AND state NOT IN ('done','failed','skipped') \
             RETURNING id \
         ) \
         SELECT (SELECT count(*) FROM stopped) AS \"runs!\", \
                (SELECT count(*) FROM abandoned) AS \"tasks!\"",
        run_id.as_uuid(),
    )
    .fetch_one(exec)
    .await?;
    Ok((row.runs > 0).then_some(Cancelled {
        runs: row.runs,
        tasks: row.tasks,
    }))
}

/// Cancel every in-flight run, optionally narrowed to one provider slug and/or one mode.
///
/// The bulk counterpart to [`cancel_run`], with the same semantics per run — including that the
/// abandoned tasks do not count as done. Both narrowings are `NULL`-tolerant, so an unnarrowed
/// call is "stop everything", which is what an operator draining the queue is asking for.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only. Nothing in flight is `Cancelled::default()`, not an error.
pub async fn cancel_active_runs<'e, E: PgExecutor<'e>>(
    exec: E,
    provider: Option<&str>,
    mode: Option<ScanMode>,
) -> DbResult<Cancelled> {
    let row = sqlx::query!(
        "WITH stopped AS ( \
             UPDATE scan_runs r SET state = 'cancelled', finished_at = now() \
             WHERE r.state IN ('queued','running') \
               AND ($2::scan_mode IS NULL OR r.mode = $2) \
               AND ($1::text IS NULL OR EXISTS ( \
                       SELECT 1 FROM providers p WHERE p.id = r.provider_id AND p.slug = $1 \
                   )) \
             RETURNING r.id \
         ), abandoned AS ( \
             UPDATE scan_tasks t SET state = 'skipped', finished_at = now() \
             WHERE t.run_id IN (SELECT id FROM stopped) \
               AND t.state NOT IN ('done','failed','skipped') \
             RETURNING t.id \
         ) \
         SELECT (SELECT count(*) FROM stopped) AS \"runs!\", \
                (SELECT count(*) FROM abandoned) AS \"tasks!\"",
        provider,
        mode as Option<ScanMode>,
    )
    .fetch_one(exec)
    .await?;
    Ok(Cancelled {
        runs: row.runs,
        tasks: row.tasks,
    })
}

// ---------------------------------------------------------------------------
// Window health and live activity
// ---------------------------------------------------------------------------

/// What the scan panel's filter actually matched, as figures rather than rows.
///
/// The console's filter used to change only which rows were listed, which answers "what
/// happened" and never "how is it going". These are the counts the same predicate produces over
/// the whole window, so a narrowed filter reports its own success rate rather than the
/// deployment's.
#[derive(Debug, Clone)]
pub struct ScanSummary {
    pub runs_total: i64,
    pub runs_queued: i64,
    pub runs_running: i64,
    pub runs_completed: i64,
    pub runs_failed: i64,
    pub runs_cancelled: i64,
    pub tasks_total: i64,
    pub tasks_done: i64,
    pub tasks_failed: i64,
    /// Failures still in the triage feed — what an operator has left to look at, as opposed to
    /// `tasks_failed`, which counts everything that ever failed in the window.
    pub failures_open: i64,
    /// Summed run wall-clock in seconds. A run still in flight counts up to `now()`, which is
    /// what makes throughput derived from this move while a scan is running.
    pub busy_seconds: f64,
    pub first_run_at: Option<OffsetDateTime>,
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
    pub slug: String,
    pub name: String,
    pub runs: i64,
    pub runs_active: i64,
    pub runs_failed: i64,
    pub tasks_done: i64,
    pub tasks_failed: i64,
    /// Failures still in the triage feed for this provider.
    pub failures_open: i64,
    pub last_run_at: Option<OffsetDateTime>,
    pub last_failure_at: Option<OffsetDateTime>,
}

/// Per-provider scan health, worst first.
///
/// Providers with neither a run in the window nor an open failure are omitted rather than listed
/// as zeroes: the table is a place to look when something is wrong, and a deployment's full
/// provider list is what the Providers panel is for.
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
        "SELECT p.slug, p.name, \
                count(r.id) AS \"runs!\", \
                count(r.id) FILTER (WHERE r.state IN ('queued','running')) AS \"runs_active!\", \
                count(r.id) FILTER (WHERE r.state = 'failed') AS \"runs_failed!\", \
                COALESCE(sum(r.done_tasks), 0) AS \"tasks_done!\", \
                COALESCE(sum(r.failed_tasks), 0) AS \"tasks_failed!\", \
                COALESCE(f.open_count, 0) AS \"failures_open!\", \
                max(r.created_at) AS last_run_at, \
                f.latest_at AS last_failure_at \
         FROM providers p \
         LEFT JOIN scan_runs r \
              ON r.provider_id = p.id \
             AND ($1::timestamptz IS NULL OR r.created_at >= $1) \
         LEFT JOIN LATERAL ( \
             SELECT count(*) AS open_count, max(t.finished_at) AS latest_at \
             FROM scan_tasks t \
             JOIN scan_runs r2 ON r2.id = t.run_id \
             WHERE r2.provider_id = p.id \
               AND t.state = 'failed' AND t.acknowledged_at IS NULL \
               AND ($1::timestamptz IS NULL OR t.finished_at >= $1) \
         ) f ON true \
         GROUP BY p.id, p.slug, p.name, f.open_count, f.latest_at \
         HAVING count(r.id) > 0 OR COALESCE(f.open_count, 0) > 0 \
         ORDER BY COALESCE(f.open_count, 0) DESC, \
                  COALESCE(sum(r.failed_tasks), 0) DESC, \
                  max(r.created_at) DESC NULLS LAST, \
                  p.slug \
         LIMIT $2",
        since,
        limit,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

pub struct RunActivity {
    pub run_id: Uuid,
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
    pub stage_done: Option<i32>,
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
    pub id: Uuid,
    pub run_id: Uuid,
    pub provider_slug: Option<String>,
    pub kind: String,
    pub state: TaskState,
    /// What the task was pointed at — the page or series path, as the planner wrote it.
    pub target: Json,
    pub error: Option<String>,
    pub attempts: i16,
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

// ---------------------------------------------------------------------------
// Where one run's time actually went
// ---------------------------------------------------------------------------

/// One task of a run, with its stage and the breakdown it recorded when it settled.
#[derive(Debug, Clone)]
pub struct TaskBreakdown {
    pub id: Uuid,
    pub kind: String,
    pub target: Json,
    pub state: TaskState,
    pub attempts: i16,
    pub worker_id: Option<String>,
    pub error: Option<String>,
    /// The stage the task is in, or — for a settled task — the one it ended in. A failure's stage
    /// is the single most useful field here: it says whether the provider stopped answering or
    /// our own ingest rejected the page.
    pub stage: Option<String>,
    pub stage_at: Option<OffsetDateTime>,
    pub stage_done: Option<i32>,
    pub stage_total: Option<i32>,
    pub stage_detail: Option<String>,
    pub created_at: Option<OffsetDateTime>,
    pub claimed_at: Option<OffsetDateTime>,
    pub finished_at: Option<OffsetDateTime>,
    /// Milliseconds between creation and claim — time nobody was working on it.
    pub wait_ms: Option<i32>,
    /// Milliseconds between claim and settle — time someone was.
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
    /// The token the statement's `ORDER BY` compares against.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Slowest => "slowest",
            Self::Recent => "recent",
        }
    }
}

/// One run's tasks, ordered so the ones that explain its duration come first.
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

/// One run's time, summed across every task that recorded a breakdown.
#[derive(Debug, Clone, Default)]
pub struct RunTelemetry {
    /// Settled tasks carrying a breakdown. Everything below is a sum over exactly these, so a run
    /// scanned before this instrumentation existed reports zero rather than a wrong total.
    pub tasks_measured: i64,
    /// Summed execution time. Exceeds the run's wall clock whenever tasks ran concurrently, which
    /// is the point — it is work performed, not time elapsed.
    pub busy_ms: i64,
    /// Summed queue wait: time tasks spent created but unclaimed.
    pub wait_ms: i64,
    pub requests: i64,
    pub fetch_ms: i64,
    /// Time spent waiting for permission to send a request. Read against `busy_ms`, this is the
    /// figure that separates "the scan is slow" from "the provider's crawl budget is small".
    pub pace_wait_ms: i64,
    pub solver_ms: i64,
    pub solver_calls: i64,
    pub throttled: i64,
}

/// Summed milliseconds for one stage across a run.
#[derive(Debug, Clone)]
pub struct StageTotal {
    pub stage: String,
    pub millis: i64,
    /// Tasks that reported this stage at all.
    pub tasks: i64,
}

/// The run's rollup and its per-stage split, as two statements.
///
/// Split rather than combined because the second one unnests a `jsonb` object per task: keeping
/// it separate leaves the rollup — the part the drawer shows first — a plain aggregate.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only. A run whose tasks recorded nothing is a zeroed
/// [`RunTelemetry`] and an empty stage list, never [`crate::DbError::NotFound`]: an old run is a
/// legitimate answer, and the drawer must render it rather than 404.
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

// ---------------------------------------------------------------------------
// Reconciliation
//
// `JetStream` is the truth for dispatch and this table is the truth for progress, and nothing
// keeps the two in step: a publish that never landed, a stream that was purged or recreated, a
// message acked by a worker that then died — each leaves a task row that is open forever and a
// run that can never settle. Nothing retries them, because from the database's side there is no
// failure to see. These four queries are what the reconciler compares and repairs against.
// ---------------------------------------------------------------------------

/// One provider lane's open work, as the database sees it.
#[derive(Debug, Clone)]
pub struct OpenLane {
    pub provider_id: ProviderId,
    pub provider_slug: String,
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
    pub id: ScanTaskId,
    pub run_id: ScanRunId,
    pub kind: String,
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

#[cfg(test)]
mod tests {
    use super::{ErrorSelector, RunSort};

    /// The two binds the clear statement derives from an [`ErrorSelector`], and the one pairing
    /// that must never happen: `Any` switching the comparison *on* would compare every failure
    /// against NULL and clear nothing, while `Absent` switching it *off* would clear the whole
    /// feed. Both are silent — the statement runs, the count is just wrong.
    #[test]
    fn each_error_selector_produces_its_own_pair_of_binds() {
        assert_eq!(
            (ErrorSelector::Any.is_narrowing(), ErrorSelector::Any.text()),
            (false, None)
        );
        assert_eq!(
            (
                ErrorSelector::Absent.is_narrowing(),
                ErrorSelector::Absent.text()
            ),
            (true, None),
            "the null group narrows, and matches through `IS NOT DISTINCT FROM NULL`"
        );
        let named = ErrorSelector::Exactly("http 503");
        assert_eq!(
            (named.is_narrowing(), named.text()),
            (true, Some("http 503"))
        );
    }

    /// A selector nobody set has to mean "every group", or a default-constructed clear would
    /// narrow to something its caller never asked for.
    #[test]
    fn the_default_selector_is_every_group() {
        assert_eq!(ErrorSelector::default(), ErrorSelector::Any);
        assert!(!ErrorSelector::default().is_narrowing());
    }

    /// The ordering tokens are compared against **string literals inside the statement**
    /// (`CASE WHEN $5::text = 'failures' …`), which no compiler relates to this enum. Renaming a
    /// token here, or mistyping one there, does not fail to build: it makes every branch of the
    /// `ORDER BY` evaluate to NULL, and the query silently falls through to the default ordering.
    /// A sort control that quietly ignores what it was asked for is the exact defect the scan
    /// panel already shipped once, so the tokens are pinned literally.
    #[test]
    fn the_ordering_tokens_are_the_ones_the_statement_compares_against() {
        assert_eq!(RunSort::Recent.token(), "recent");
        assert_eq!(RunSort::Oldest.token(), "oldest");
        assert_eq!(RunSort::Failures.token(), "failures");
        assert_eq!(RunSort::Duration.token(), "duration");
    }

    /// Two orderings sharing a token would make one of them unreachable — the statement would
    /// take whichever branch the shared literal names, for both.
    #[test]
    fn no_two_orderings_share_a_token() {
        let tokens = [
            RunSort::Recent.token(),
            RunSort::Oldest.token(),
            RunSort::Failures.token(),
            RunSort::Duration.token(),
        ];
        let mut seen = tokens.to_vec();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), tokens.len(), "{tokens:?} contains a duplicate");
    }

    /// The default has to be the one ordering the statement needs *no* branch for: every `CASE`
    /// collapses to NULL and the trailing `created_at DESC` decides. A default that named a
    /// branch would make "no sort asked for" mean something the API does not document.
    #[test]
    fn the_default_ordering_is_newest_first() {
        assert_eq!(RunSort::default(), RunSort::Recent);
        assert_eq!(RunSort::default().token(), "recent");
    }
}
