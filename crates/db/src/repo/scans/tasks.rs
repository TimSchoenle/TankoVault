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

/// Creates a queued task and returns its id, or `None` when an identical task (`run_id`, `kind`,
/// `target`) already exists.
///
/// The `ON CONFLICT DO NOTHING` on the `scan_tasks_run_kind_target` unique index makes fan-out
/// idempotent under at-least-once delivery: a redelivered `catalog_page` gets `None` back, and the
/// caller skips the duplicate `add_total_tasks` and publish (`docs/ARCHITECTURE.md` §12). `None`
/// is the expected outcome of a redelivery, not a conflict.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only, including the foreign-key violation of an unknown `run_id`.
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

/// Creates many queued tasks of the same `kind` in one statement, returning the `(id, target)`
/// pairs that were **actually inserted**.
///
/// Behaves as [`create_task`] per target, idempotency included, in one round trip. Targets
/// already present, and duplicates within the batch, are absent from the result, so the caller
/// publishes only new tasks. Callers chunk large fan-outs; see `CATALOG_FANOUT_CHUNK` in the
/// worker.
///
/// An empty result is not a failure: it is also what a fully redelivered fan-out returns. An
/// empty `targets` issues no statement.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only, including the foreign-key violation of an unknown `run_id`.
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

/// Marks a task claimed by a worker, returning whether the work should actually be done.
///
/// A task that has already **settled** is not re-claimed, which keeps the run counters honest
/// under at-least-once delivery. Re-claiming would reopen the settle guard and count one task
/// twice, and `finalize_if_complete` would finalise the run while other tasks still run.
/// `crates/db/tests/repo_scans.rs` pins this.
///
/// A task whose **run has been cancelled** is not claimed either. `JetStream` holds the message
/// independently, so this is the only place a cancellation can be honoured. `false` means
/// "settle this message and do nothing"; a worker that ignored it would keep crawling a provider
/// an operator told it to stop. An unknown task is `false` too.
///
/// Claiming also opens the stage at [`tankovault_domain::ScanStage::Starting`] and clears any
/// stage a previous delivery left, so a redelivered task never shows its abandoned progress.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
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

/// Claims the oldest queued task of a run with `FOR UPDATE SKIP LOCKED`, the durable fallback
/// when the broker is unavailable.
///
/// `None` means no claimable task *right now*: nothing queued, every queued row locked by another
/// worker, or an unknown `run_id`. The caller polls and must not read `None` as a finished run.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
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

/// Records which stage a claimed task is in, how far through it is, and against what.
///
/// `stage_at` moves only when the stage changes, so "in this stage since" stays stable while a
/// stage reports progress and can show a stage that has stopped advancing.
///
/// Guarded on `state = 'claimed'`: a stage write that lost a race with the settle would reopen a
/// live stage on a finished row. An unknown or settled task writes nothing; this is diagnostic,
/// and a caller must never fail a scan over it.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
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
    /// Returns the telemetry blob as the column stores it, or `None` when it cannot be encoded,
    /// which this type's shape makes unreachable and which must never fail a settle.
    fn payload(this: Option<&Self>) -> Option<Json> {
        this.and_then(|outcome| serde_json::to_value(outcome.timings).ok())
    }
}

/// Marks a task done and increments the run's done counter in one statement, recording what the
/// work cost.
///
/// The guard excludes **every** terminal state, not just `done`, so a task is counted once, on the
/// first settle. Otherwise a redelivery failing after an earlier success would add a
/// `failed_tasks` count on top, taking the sum above `total_tasks`; see [`claim_task`]. An unknown
/// or settled task touches nothing, which is the settle-once property.
///
/// `wait_ms` keeps its **first** value, so a retried task reports how long it originally waited.
///
/// Always propagate the error: a failure leaves the task unsettled and the run one count short,
/// and only a redelivery recovers it, which needs the worker to decline the ack.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
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

/// Marks a task failed with an error, incrementing the run's failed counter and recording what the
/// attempt cost before it failed.
///
/// The settle-once guard of [`complete_task`]: a settled task keeps its first state and count,
/// so a redelivery cannot turn a completed task into a failed one. The row's stage is left alone,
/// since the stage a task died in is the most useful field on a failure.
///
/// Propagate the error as for [`complete_task`]: a caller that only logs it loses both the reason
/// the task failed and the count that ends the run, which move in this one statement.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
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

/// Returns recent failed tasks, narrowed by provider and time window: the operator's triage feed
/// for stuck providers and broken selectors (`docs/ARCHITECTURE.md` §17.2.7).
///
/// `include_cleared` reopens acknowledged failures, which are excluded by default but never
/// deleted. The join to `providers` is `LEFT`, so a run whose provider was deleted still appears
/// with `provider_slug: None`.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
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
    /// Task kinds this error struck, sorted: a `series` failure and a `catalog_page` failure
    /// with the same message are different problems.
    pub kinds: Vec<String>,
    /// The most recent of them.
    pub latest_at: Option<OffsetDateTime>,
}

/// Returns failed tasks collapsed by their error text, worst first.
///
/// Twelve rows of one broken selector are one problem, so grouping lets the panel answer "what
/// is wrong", not "what happened most recently".
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
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
/// Three-way: an `Option<&str>` cannot say "the failures that recorded no error", which would
/// read as "any error" and turn clearing one quiet group into clearing the whole feed.
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
    /// Returns whether the statement should compare on the error at all.
    const fn is_narrowing(self) -> bool {
        !matches!(self, Self::Any)
    }

    /// Returns the text to compare against; `None` both for [`Self::Any`], where the comparison is
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

/// Clears the selected failures out of the triage feed.
///
/// Returns how many rows this call acknowledged. Already-cleared rows are excluded, so the count
/// is what *this* operator hid, and two operators clearing the same feed do not both claim it.
///
/// This never deletes: the task keeps its `failed` state, its error and its counter contribution,
/// so a cleared feed still reconciles against the run history and the audit trail. An operator
/// dismissing noise must not be able to erase the record of an outage.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn clear_failures<'e, E: PgExecutor<'e>>(
    exec: E,
    selector: &FailureSelector<'_>,
) -> DbResult<u64> {
    // `IS NOT DISTINCT FROM`, since the group being cleared may be the one whose error is NULL,
    // and `= NULL` matches nothing.
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

/// Marks a task skipped (unchanged content, no work needed) and counts it as done.
///
/// The settle-once guard of [`complete_task`], which stops a failed task from later being
/// skipped and counted as done too. A skip increments `done_tasks`, so an unchanged-content
/// re-scan can finalise its run. The notes on [`complete_task`] apply unchanged.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
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
    /// feed. Both are silent: the statement runs, and only the count is wrong.
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
