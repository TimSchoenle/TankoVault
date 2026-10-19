//! The operator's re-verification of stored mappings: the work list and the single state row
//! (`mapping_reverify_state`) an operator follows a run through.

use crate::error::DbResult;
use sqlx::PgExecutor;
use tankovault_domain::SeriesId;
use time::OffsetDateTime;
use uuid::Uuid;

/// How long a run may go without a heartbeat before its claim is considered abandoned.
///
/// Comfortably above the gap between two heartbeats, which is one page of rate-limited lookups.
const ABANDONED_AFTER_SECS: f64 = 600.0;

/// Counters of a run, as stored.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReverifyCounts {
    /// Mappings looked up.
    pub checked: i32,
    /// Of those, the ones whose record does not identify the series.
    pub failed: i32,
    /// Of the failed, the ones removed (zero on a dry run).
    pub removed: i32,
    /// Lookups that errored, left untouched.
    pub unreachable: i32,
}

/// The single row of `mapping_reverify_state`.
#[derive(Debug, Clone)]
pub struct ReverifyState {
    /// Whether a run holds the claim.
    pub running: bool,
    /// Whether the current or last run only reported.
    pub dry_run: bool,
    /// When it began.
    pub started_at: Option<OffsetDateTime>,
    /// When it released the claim, `None` while running.
    pub finished_at: Option<OffsetDateTime>,
    /// Its counters.
    pub counts: ReverifyCounts,
    /// The failing mappings of the last finished run, capped.
    pub failures: serde_json::Value,
    /// Why it failed, `None` when it did not.
    pub error: Option<String>,
}

/// Claim the state row for a new run. Returns `false` when a live run holds it.
///
/// A claim whose heartbeat is older than ten minutes is taken over: its replica died
/// without releasing it, and refusing forever would need a database edit to recover.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn begin_reverify<'e, E: PgExecutor<'e>>(exec: E, dry_run: bool) -> DbResult<bool> {
    let claimed = sqlx::query_scalar!(
        "UPDATE mapping_reverify_state \
            SET running = true, dry_run = $1, started_at = now(), heartbeat_at = now(), \
                finished_at = NULL, checked = 0, failed = 0, removed = 0, unreachable = 0, \
                failures = '[]'::jsonb, error = NULL \
          WHERE id AND (NOT running OR heartbeat_at < now() - make_interval(secs => $2)) \
         RETURNING id",
        dry_run,
        ABANDONED_AFTER_SECS,
    )
    .fetch_optional(exec)
    .await?;
    Ok(claimed.is_some())
}

/// Publish the counters of a run in flight, which also renews its claim.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn record_reverify_progress<'e, E: PgExecutor<'e>>(
    exec: E,
    counts: ReverifyCounts,
) -> DbResult<()> {
    sqlx::query!(
        "UPDATE mapping_reverify_state \
            SET checked = $1, failed = $2, removed = $3, unreachable = $4, heartbeat_at = now() \
          WHERE id",
        counts.checked,
        counts.failed,
        counts.removed,
        counts.unreachable,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Release the claim and record how the run ended. **Must run on the failure path too.**
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn finish_reverify<'e, E: PgExecutor<'e>>(
    exec: E,
    counts: ReverifyCounts,
    failures: &serde_json::Value,
    error: Option<&str>,
) -> DbResult<()> {
    sqlx::query!(
        "UPDATE mapping_reverify_state \
            SET running = false, finished_at = now(), heartbeat_at = now(), \
                checked = $1, failed = $2, removed = $3, unreachable = $4, \
                failures = $5, error = $6 \
          WHERE id",
        counts.checked,
        counts.failed,
        counts.removed,
        counts.unreachable,
        failures,
        error,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// The current or last run.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; the row is seeded by its migration.
pub async fn reverify_state<'e, E: PgExecutor<'e>>(exec: E) -> DbResult<ReverifyState> {
    let row = sqlx::query!(
        "SELECT running, dry_run, started_at, finished_at, checked, failed, removed, \
                unreachable, failures, error \
           FROM mapping_reverify_state WHERE id",
    )
    .fetch_one(exec)
    .await?;
    Ok(ReverifyState {
        running: row.running,
        dry_run: row.dry_run,
        started_at: row.started_at,
        finished_at: row.finished_at,
        counts: ReverifyCounts {
            checked: row.checked,
            failed: row.failed,
            removed: row.removed,
            unreachable: row.unreachable,
        },
        failures: row.failures,
        error: row.error,
    })
}

/// One page of `provider`'s mappings in series-id order, after `after`.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; the end of the list is an empty `Vec`.
pub async fn list_mappings_after<'e, E: PgExecutor<'e>>(
    exec: E,
    provider: &str,
    after: Option<SeriesId>,
    limit: i64,
) -> DbResult<Vec<(SeriesId, String)>> {
    let rows = sqlx::query!(
        "SELECT series_id, external_id FROM sync_mappings \
          WHERE provider = $1 AND series_id > $2 \
          ORDER BY series_id \
          LIMIT $3",
        provider,
        after.map_or(Uuid::nil(), SeriesId::as_uuid),
        limit,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (SeriesId::from_uuid(r.series_id), r.external_id))
        .collect())
}
