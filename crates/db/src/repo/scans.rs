//! Scan run + task repository. The task row is the truth for **progress and audit**;
//! the `JetStream` stream is the truth for **dispatch** (design §2). A durable
//! `SELECT ... FOR UPDATE SKIP LOCKED` claim path is provided as the fallback/audit
//! mechanism when the broker is unavailable.
//!
//! # Settle once
//!
//! `scan_runs.done_tasks` / `.failed_tasks` are counts of *tasks*, not of settle calls, and
//! [`finalize_if_complete`] compares their sum against `total_tasks` to decide a run is over.
//! That only holds if a task can be counted at most once, so every statement that settles a task
//! ([`complete_task`], [`fail_task`], [`skip_task`]) and the claim that precedes it
//! ([`claim_task`]) excludes the same three terminal states. Delivery is at-least-once, so this
//! is a live path, not a theoretical one: see `crates/db/tests/repo_scans.rs`.

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

/// List recent runs (console overview).
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. A deployment that has never
/// scanned is an empty `Vec`, not [`crate::DbError::NotFound`].
pub async fn list_recent_runs<'e, E: PgExecutor<'e>>(
    exec: E,
    limit: i64,
) -> DbResult<Vec<ScanRun>> {
    let rows = sqlx::query_as!(
        RunRow,
        "SELECT id, provider_id, mode AS \"mode: ScanMode\", state AS \"state: RunState\", \
         total_tasks, done_tasks, failed_tasks, started_at, finished_at, created_at \
         FROM scan_runs ORDER BY created_at DESC LIMIT $1",
        limit,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows.into_iter().map(ScanRun::from).collect())
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
}

/// The most recently failed tasks across all runs, newest first — the operator's triage
/// feed for stuck providers / broken selectors (design §17.2.7).
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. Nothing failed is an empty
/// `Vec`, which is the feed's goal state. The join to `providers` is a `LEFT` join on purpose —
/// a run whose provider has since been deleted still appears, with `provider_slug: None`, rather
/// than dropping out of the triage feed exactly when an operator deleted the thing that was
/// failing.
pub async fn recent_failed_tasks<'e, E: PgExecutor<'e>>(
    exec: E,
    limit: i64,
) -> DbResult<Vec<FailedTaskView>> {
    let rows = sqlx::query_as!(
        FailedTaskView,
        "SELECT t.id, t.run_id, p.slug AS \"provider_slug?\", r.mode::text AS \"mode!\", \
                t.kind, t.error, t.attempts, t.finished_at \
         FROM scan_tasks t \
         JOIN scan_runs r ON r.id = t.run_id \
         LEFT JOIN providers p ON p.id = r.provider_id \
         WHERE t.state = 'failed' \
         ORDER BY t.finished_at DESC NULLS LAST \
         LIMIT $1",
        limit,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

/// Mark a task skipped (unchanged content, no work needed) and count it as done.
///
/// Same settle-once guard as [`complete_task`]. `failed` joined the exclusion list here: it was
/// the one terminal state this statement did not exclude, so a failed task could be skipped
/// afterwards and add a `done_tasks` count next to its `failed_tasks` one.
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
