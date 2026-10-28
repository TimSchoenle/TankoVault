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

/// Cancels one in-flight run and abandons every task it still had outstanding.
///
/// Returns `None` when the run was already terminal or does not exist, since two operators
/// pressing the same button is ordinary.
///
/// The queued **messages** belong to `JetStream` and cannot be unpublished. A worker that pulls
/// one after this call gets `false` from [`claim_task`], which is where cancellation is honoured:
/// the provider stops at its next task boundary, and a task already in flight runs to its end
/// with a no-op settle.
///
/// Abandoned tasks are `skipped` but, unlike [`skip_task`], do **not** bump `done_tasks`:
/// counting "told to stop" as done would show a cancelled run as completed.
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

/// Cancels every in-flight run, optionally narrowed to one provider slug, one mode, or both.
///
/// The bulk counterpart to [`cancel_run`], with the same semantics per run. Both narrowings are
/// `NULL`-tolerant, so an unnarrowed call stops everything. Nothing in flight is
/// `Cancelled::default()`.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
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
