//! Scan tasks: creation, claiming, settling, and the failure feed built from them.

use crate::error::DbResult;
use serde_json::Value as Json;
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::{ScanRunId, ScanStage, ScanTask, ScanTaskId, StageTimings, TaskState};
use time::OffsetDateTime;
use uuid::Uuid;

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
/// re-enqueuing every series (`docs/ARCHITECTURE.md` §12). Relies on the `scan_tasks_run_kind_target`
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
    /// Where inside the task that wall clock went.
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
    /// The failed task.
    pub id: Uuid,
    /// The run it belonged to.
    pub run_id: Uuid,
    /// Provider slug of the owning run (`None` if the provider was since deleted).
    pub provider_slug: Option<String>,
    /// Cadence of the owning run, as a text-cast token.
    pub mode: String,
    /// What the task was asked to do.
    pub kind: String,
    /// Why it failed, `None` for a task that recorded no message.
    pub error: Option<String>,
    /// Claims taken before it gave up.
    pub attempts: i16,
    /// When it gave up, which is what the feed windows on.
    #[serde(with = "time::serde::rfc3339::option")]
    pub finished_at: Option<OffsetDateTime>,
    /// When an operator cleared this failure from the triage feed, if they have.
    #[serde(with = "time::serde::rfc3339::option")]
    pub acknowledged_at: Option<OffsetDateTime>,
}

/// Recent failed tasks, narrowed by provider and time window — the operator's triage feed for
/// stuck providers and broken selectors (`docs/ARCHITECTURE.md` §17.2.7).
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
    /// Failures sharing this error, cleared ones counted only when the caller asked for them.
    pub count: i64,
    /// How many of `count` an operator has already cleared. Non-zero only when the caller asked
    /// for cleared failures back.
    pub cleared: i64,
    /// Provider slugs affected, sorted. A provider deleted since is omitted, not `null`.
    pub providers: Vec<String>,
    /// Task kinds this error struck, sorted — a `series` failure and a `catalog_page` failure
    /// with the same message are different problems.
    pub kinds: Vec<String>,
    /// The most recent of them.
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
    /// One provider's failures, by slug.
    pub provider: Option<&'a str>,
    /// Inclusive lower bound on the failure's `finished_at`.
    pub since: Option<OffsetDateTime>,
    /// One run's failures.
    pub run_id: Option<ScanRunId>,
    /// Which error text to clear, and whether a missing one counts.
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

#[cfg(test)]
mod tests {
    use super::ErrorSelector;

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
}
