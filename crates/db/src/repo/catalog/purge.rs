//! The single row recording the detached catalogue purge: its claim, its progress, and how the
//! last run ended. The batches themselves are in [`super::maintenance`].

use super::maintenance::DeletionReport;
use crate::error::DbResult;
use sqlx::PgExecutor;
use time::OffsetDateTime;
use uuid::Uuid;

/// How long a claim survives without a batch committing before another run may break it.
///
/// Shared by the claim and the read so the console never offers a button no claim could be
/// granted for. Generous against a batch, which is sized to take seconds.
const LEASE_SECS: f64 = 300.0;

/// How much of the catalogue a purge takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PurgeTarget {
    /// Every chapter; series and sources survive for the next scan to refill.
    Chapters,
    /// Every series and everything hanging off one.
    Everything,
}

impl PurgeTarget {
    /// The token stored in `catalogue_purge_state.scope`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Chapters => "chapters",
            Self::Everything => "everything",
        }
    }

    fn parse(token: &str) -> Option<Self> {
        match token {
            "chapters" => Some(Self::Chapters),
            "everything" => Some(Self::Everything),
            _ => None,
        }
    }
}

/// Why a purge run stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PurgeStop {
    /// Nothing of the purged kind is left.
    Done,
    /// An operator asked it to stop.
    Cancelled,
    /// A batch removed nothing while rows remained.
    Stalled,
    /// A batch returned an error.
    Failed,
    /// The process running it died without releasing the claim. Only ever read, never written.
    Interrupted,
}

impl PurgeStop {
    /// The token stored in `catalogue_purge_state.stopped`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Cancelled => "cancelled",
            Self::Stalled => "stalled",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
        }
    }

    fn parse(token: &str) -> Option<Self> {
        match token {
            "done" => Some(Self::Done),
            "cancelled" => Some(Self::Cancelled),
            "stalled" => Some(Self::Stalled),
            "failed" => Some(Self::Failed),
            "interrupted" => Some(Self::Interrupted),
            _ => None,
        }
    }
}

/// A granted claim on the purge. Every advance and release is conditioned on its token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PurgeClaim {
    /// The token.
    pub claim_id: Uuid,
}

/// The purge's state as the console reads it.
#[derive(Debug, Clone)]
pub struct PurgeState {
    /// Whether a run holds the claim and is still stamping it.
    pub running: bool,
    /// The scope of the current or last run; `None` before the first.
    pub scope: Option<PurgeTarget>,
    /// Who started the current or last run.
    pub started_by: Option<Uuid>,
    /// When the current or last run took the claim.
    pub started_at: Option<OffsetDateTime>,
    /// When the last run released the claim.
    pub finished_at: Option<OffsetDateTime>,
    /// Whether a cancel has been asked of the running purge.
    pub cancel_requested: bool,
    /// What the current or last run removed so far.
    pub removed: DeletionReport,
    /// Rows of the purged kind left after the last committed batch.
    pub remaining: Option<i64>,
    /// Why the last run stopped; `None` while one is running or before the first.
    pub stopped: Option<PurgeStop>,
    /// Why the last run failed.
    pub error: Option<String>,
}

/// Claim the purge and reset the row to the start of a run.
///
/// `None` when a live run already holds the claim. A claim whose heartbeat is older than the
/// lease is broken and re-granted, which is how a purge interrupted by a restart is resumed.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn claim_purge<'e, E: PgExecutor<'e>>(
    exec: E,
    target: PurgeTarget,
    actor: Uuid,
) -> DbResult<Option<PurgeClaim>> {
    let granted = sqlx::query_scalar!(
        "UPDATE catalogue_purge_state \
            SET running           = true, \
                claim_id          = $1, \
                heartbeat_at      = now(), \
                started_at        = now(), \
                finished_at       = NULL, \
                scope             = $2, \
                started_by        = $3, \
                cancel_requested  = false, \
                series_removed    = 0, \
                sources_removed   = 0, \
                chapters_removed  = 0, \
                watchlist_removed = 0, \
                progress_removed  = 0, \
                remaining         = NULL, \
                stopped           = NULL, \
                error             = NULL \
          WHERE id \
            AND (NOT running \
                 OR heartbeat_at IS NULL \
                 OR heartbeat_at < now() - make_interval(secs => $4::double precision)) \
      RETURNING claim_id AS \"claim_id!\"",
        Uuid::new_v4(),
        target.as_str(),
        actor,
        LEASE_SECS,
    )
    .fetch_optional(exec)
    .await?;
    Ok(granted.map(|claim_id| PurgeClaim { claim_id }))
}

/// Record the run's running totals and stamp the lease.
///
/// Returns whether the run should carry on: `false` once a cancel has been asked, or once the
/// claim has been superseded and this run no longer owns the row.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn advance_purge<'e, E: PgExecutor<'e>>(
    exec: E,
    claim: PurgeClaim,
    removed: DeletionReport,
    remaining: i64,
) -> DbResult<bool> {
    let cancel = sqlx::query_scalar!(
        "UPDATE catalogue_purge_state \
            SET heartbeat_at      = now(), \
                series_removed    = $2, \
                sources_removed   = $3, \
                chapters_removed  = $4, \
                watchlist_removed = $5, \
                progress_removed  = $6, \
                remaining         = $7 \
          WHERE id AND claim_id = $1 AND running \
      RETURNING cancel_requested",
        claim.claim_id,
        removed.series,
        removed.sources,
        removed.chapters,
        removed.watchlist_entries,
        removed.progress_rows,
        remaining,
    )
    .fetch_optional(exec)
    .await?;
    Ok(cancel == Some(false))
}

/// Release the claim, recording the run's final totals and why it stopped.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn finish_purge<'e, E: PgExecutor<'e>>(
    exec: E,
    claim: PurgeClaim,
    removed: DeletionReport,
    remaining: Option<i64>,
    stopped: PurgeStop,
    error: Option<&str>,
) -> DbResult<()> {
    sqlx::query!(
        "UPDATE catalogue_purge_state \
            SET running           = false, \
                heartbeat_at      = now(), \
                finished_at       = now(), \
                series_removed    = $2, \
                sources_removed   = $3, \
                chapters_removed  = $4, \
                watchlist_removed = $5, \
                progress_removed  = $6, \
                remaining         = COALESCE($7, remaining), \
                stopped           = $8, \
                error             = $9 \
          WHERE id AND claim_id = $1",
        claim.claim_id,
        removed.series,
        removed.sources,
        removed.chapters,
        removed.watchlist_entries,
        removed.progress_rows,
        remaining,
        stopped.as_str(),
        error,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Ask the live run to stop after its current batch. `false` when no run is live.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn request_purge_cancel<'e, E: PgExecutor<'e>>(exec: E) -> DbResult<bool> {
    let hit = sqlx::query!(
        "UPDATE catalogue_purge_state SET cancel_requested = true \
          WHERE id AND running \
            AND heartbeat_at >= now() - make_interval(secs => $1::double precision)",
        LEASE_SECS,
    )
    .execute(exec)
    .await?
    .rows_affected();
    Ok(hit > 0)
}

/// Read the purge's state, resolving `running` against the lease the claim is granted under.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; the row is created by migration 0065 and cannot be absent.
pub async fn read_purge_state<'e, E: PgExecutor<'e>>(exec: E) -> DbResult<PurgeState> {
    let row = sqlx::query!(
        "SELECT running \
                AND heartbeat_at IS NOT NULL \
                AND heartbeat_at >= now() - make_interval(secs => $1::double precision) \
                  AS \"live!\", \
                running AS \"claimed!\", \
                scope, started_by, started_at, finished_at, cancel_requested, \
                series_removed, sources_removed, chapters_removed, watchlist_removed, \
                progress_removed, remaining, stopped, error \
         FROM catalogue_purge_state WHERE id",
        LEASE_SECS,
    )
    .fetch_one(exec)
    .await?;
    // A claim still held past its lease belongs to a process that died mid-run.
    let stopped = if row.claimed && !row.live {
        Some(PurgeStop::Interrupted)
    } else {
        row.stopped.as_deref().and_then(PurgeStop::parse)
    };
    Ok(PurgeState {
        running: row.live,
        scope: row.scope.as_deref().and_then(PurgeTarget::parse),
        started_by: row.started_by,
        started_at: row.started_at,
        finished_at: row.finished_at,
        cancel_requested: row.cancel_requested,
        removed: DeletionReport {
            series: row.series_removed,
            sources: row.sources_removed,
            chapters: row.chapters_removed,
            watchlist_entries: row.watchlist_removed,
            progress_rows: row.progress_removed,
        },
        remaining: row.remaining,
        stopped,
        error: row.error,
    })
}
