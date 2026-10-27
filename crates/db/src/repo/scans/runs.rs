//! Scan runs: creation, listing, the failure streak, and the run's own lifecycle.

use crate::error::{DbError, DbResult};
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::{ProviderId, RunState, ScanMode, ScanRun, ScanRunId};
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
    /// The run row as stored.
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
    /// Oldest first.
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
    /// Restricts to one cadence.
    pub mode: Option<ScanMode>,
    /// Restricts to one lifecycle state.
    pub state: Option<RunState>,
    /// Inclusive lower bound on the run's creation time.
    pub since: Option<OffsetDateTime>,
    /// Ordering, applied over the whole matched window rather than over the page.
    pub sort: RunSort,
}

/// One page of scan runs, plus how many the filter matches in total.
#[derive(Debug, Clone)]
pub struct RunPage {
    /// The page, in the filter's order.
    pub items: Vec<RunListing>,
    /// Runs the filter matched, ignoring `limit` and `offset`.
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
pub(super) const FAILURE_STREAK_WINDOW: i64 = 32;

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

/// Settles a run, but only if it is still `running` and every planned task has settled.
///
/// "Every planned task" is `done_tasks + failed_tasks >= total_tasks` with `total_tasks > 0`.
/// The terminal state is [`RunState::Failed`] only when every task failed; a partially failed
/// run still completed.
///
/// Answers `Some` only for the call that performed the transition, and `None` when the run was
/// not yet complete or another actor got there first. That is what lets the progress aggregator
/// emit exactly one terminal event per run rather than looping (`docs/ARCHITECTURE.md` §8, §12).
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

#[cfg(test)]
mod tests {
    use super::RunSort;

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
