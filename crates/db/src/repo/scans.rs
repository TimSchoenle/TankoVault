//! Scan run + task repository. The task row is the truth for progress/audit; `JetStream` is
//! the truth for dispatch, with a `FOR UPDATE SKIP LOCKED` claim path as fallback.
//!
//! Settle-once: every claim/settle statement excludes the same three terminal states, so a
//! task counts toward `done_tasks`/`failed_tasks` at most once (`tests/repo_scans.rs` pins this).

use crate::error::{DbError, DbResult};
use serde_json::Value as Json;
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::{
    ProviderId, RunState, ScanMode, ScanRun, ScanRunId, ScanTask, ScanTaskId, TaskState,
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

/// Mark a task claimed by a worker (the normal path: the worker already has the message
/// from `JetStream` and records the claim for audit/progress).
///
/// A task that has already **settled** is not re-claimed. That guard is what makes the run
/// counters honest under at-least-once delivery: without it a redelivery of a task the worker
/// already completed put the row back into `claimed`, which re-opened
/// [`complete_task`]'s `state <> 'done'` guard and incremented `done_tasks` a second time for
/// one task. `finalize_if_complete` fires on `done_tasks + failed_tasks >= total_tasks`, so two
/// counts for one task finalise the run — and emit its single terminal event — while other
/// tasks are still running.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. Everything the guard above
/// describes is silent: an unknown task, and a task that has already settled, are both `Ok(())`
/// with no row touched, and this function returns no count that would distinguish them from a
/// successful claim. That is a deliberate consequence of the settle-once rule — the worker is
/// holding the `JetStream` message either way and will do the work and settle it, where the same
/// guard applies again — but it means a caller cannot use this to decide whether to *skip* the
/// work. `crates/db/tests/repo_scans.rs` pins the counter behaviour that depends on it.
pub async fn claim_task<'e, E: PgExecutor<'e>>(
    exec: E,
    task_id: ScanTaskId,
    worker_id: &str,
) -> DbResult<()> {
    sqlx::query!(
        "UPDATE scan_tasks SET state = 'claimed', worker_id = $2, claimed_at = now(), \
         attempts = attempts + 1 \
         WHERE id = $1 AND state NOT IN ('done','failed','skipped')",
        task_id.as_uuid(),
        worker_id,
    )
    .execute(exec)
    .await?;
    Ok(())
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

/// Mark a task done and increment the run's done counter, atomically per statement.
///
/// The guard excludes **every** terminal state, not just `done`: a task is counted once, on the
/// first settle that reaches it. Guarding only `state <> 'done'` let a redelivery that failed
/// after an earlier success add a `failed_tasks` count on top of the `done_tasks` one, taking
/// `done_tasks + failed_tasks` above `total_tasks` — see [`claim_task`].
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
pub async fn complete_task<'e, E: PgExecutor<'e>>(exec: E, task_id: ScanTaskId) -> DbResult<()> {
    // Two statements would race on the counter under concurrency; a CTE keeps them in
    // one round trip and lets each worker's increment commit independently.
    sqlx::query!(
        "WITH done AS ( \
            UPDATE scan_tasks SET state = 'done', finished_at = now() \
            WHERE id = $1 AND state NOT IN ('done','failed','skipped') RETURNING run_id \
         ) \
         UPDATE scan_runs SET done_tasks = done_tasks + 1 \
         WHERE id = (SELECT run_id FROM done)",
        task_id.as_uuid(),
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Mark a task failed with an error, incrementing the run's failed counter.
///
/// Same settle-once guard as [`complete_task`]: an already-settled task keeps the state and the
/// count it first reached, so a redelivery cannot turn a completed task into a failed one and
/// have the run count it twice.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable, and the same silent-no-op and
/// stranded-run notes as [`complete_task`] apply unchanged. One difference is worth stating:
/// this runs on a path that is *already* handling a failure, so a caller that logs rather than
/// propagates loses the record of why the task failed **and** the count that ends the run. Both
/// halves are in this one statement precisely so neither can be lost without the other.
pub async fn fail_task<'e, E: PgExecutor<'e>>(
    exec: E,
    task_id: ScanTaskId,
    error: &str,
) -> DbResult<()> {
    sqlx::query!(
        "WITH failed AS ( \
            UPDATE scan_tasks SET state = 'failed', error = $2, finished_at = now() \
            WHERE id = $1 AND state NOT IN ('done','failed','skipped') RETURNING run_id \
         ) \
         UPDATE scan_runs SET failed_tasks = failed_tasks + 1 \
         WHERE id = (SELECT run_id FROM failed)",
        task_id.as_uuid(),
        error,
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

/// The task-level shape of one run that is still in flight.
#[derive(Debug, Clone)]
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
                count(DISTINCT t.worker_id) FILTER (WHERE t.state = 'claimed') AS \"workers!\" \
         FROM scan_runs r \
         LEFT JOIN scan_tasks t ON t.run_id = r.id \
         WHERE r.state IN ('queued','running') \
         GROUP BY r.id",
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
