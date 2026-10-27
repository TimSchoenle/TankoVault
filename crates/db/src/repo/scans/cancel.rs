//! Cancelling a run, or every active run, and settling its open tasks.

use crate::error::DbResult;
use sqlx::PgExecutor;
use tankovault_domain::{ScanMode, ScanRunId};

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
///
/// [`claim_task`]: super::claim_task
/// [`skip_task`]: super::skip_task
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
