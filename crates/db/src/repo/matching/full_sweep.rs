//! The single row recording the on-demand exhaustive duplicate sweep: its claim, its progress,
//! and how the last run ended.

use crate::error::DbResult;
use sqlx::PgExecutor;
use time::OffsetDateTime;
use uuid::Uuid;

/// How long a claim survives without a round completing before another run may break it.
///
/// Sized against a round, which is one budgeted sweep and takes seconds: five minutes without
/// one means the process holding the claim is gone rather than busy.
///
/// Not a parameter. Both the service that takes the claim and the API that reports whether one
/// is held resolve staleness against it, and a lease they disagreed about would have the console
/// offering a button no claim could be granted for — or withholding one that could.
const LEASE_SECS: f64 = 300.0;

/// A granted claim on the exhaustive sweep.
///
/// Every write that advances or releases it carries the token, so a run whose lease expired
/// while it was still going cannot write over the run that replaced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FullSweepClaim {
    pub claim_id: Uuid,
}

/// The counters one exhaustive run accumulates, in the order the console reads them.
///
/// The same set a single sweep reports (`MergeSweepView` in `tankovault-contracts`), minus
/// `chains_deferred`: that one means "the *last pass* left work behind", and a run which keeps
/// drawing rounds until the shortlists are dry has resolved it by definition.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FullSweepCounters {
    pub pairs_examined: i64,
    pub auto_merged: i64,
    pub queued: i64,
    pub requeued: i64,
    pub reopened: i64,
    pub withdrawn: i64,
    pub distinct: i64,
    pub deferred: i64,
    pub blocked: i64,
}

/// The exhaustive sweep's state as the console reads it.
#[derive(Debug, Clone)]
pub struct FullSweepState {
    /// Whether a run holds the claim *and* is still stamping it. A holder whose heartbeat has
    /// gone stale reads as not running, because that is what an operator needs to know: the
    /// button is pressable again.
    pub running: bool,
    pub started_at: Option<OffsetDateTime>,
    /// When the last run released the claim; absent while one is running.
    pub finished_at: Option<OffsetDateTime>,
    pub rounds: i32,
    pub counters: FullSweepCounters,
    /// Why the last run stopped — `exhausted`, `merge_ceiling`, `round_cap` or `failed`. Only
    /// `exhausted` means the catalogue was walked to the end.
    pub stopped: Option<String>,
    pub error: Option<String>,
}

/// Claim the exhaustive sweep and reset the row to the start of a run.
///
/// Returns `None` when a live run already holds the claim — the correct response to which is to
/// report that, not to queue behind it: the other run is doing this one's work.
///
/// A claim whose `heartbeat_at` is older than [`LEASE_SECS`] is broken and re-granted. Without
/// that, a run killed between two rounds would hold the claim forever and no operator could
/// start another.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn claim_full_sweep<'e, E: PgExecutor<'e>>(exec: E) -> DbResult<Option<FullSweepClaim>> {
    let claim_id = Uuid::new_v4();
    let granted = sqlx::query_scalar!(
        "UPDATE merge_full_sweep_state \
            SET running        = true, \
                claim_id       = $1, \
                heartbeat_at   = now(), \
                started_at     = now(), \
                finished_at    = NULL, \
                rounds         = 0, \
                pairs_examined = 0, \
                auto_merged    = 0, \
                queued         = 0, \
                requeued       = 0, \
                reopened       = 0, \
                withdrawn      = 0, \
                distinct_pairs = 0, \
                deferred       = 0, \
                blocked        = 0, \
                stopped        = NULL, \
                error          = NULL \
          WHERE id \
            AND (NOT running \
                 OR heartbeat_at IS NULL \
                 OR heartbeat_at < now() - make_interval(secs => $2::double precision)) \
      RETURNING claim_id AS \"claim_id!\"",
        claim_id,
        LEASE_SECS,
    )
    .fetch_optional(exec)
    .await?;
    Ok(granted.map(|claim_id| FullSweepClaim { claim_id }))
}

/// Record what the run has done after `rounds` rounds, and stamp the lease.
///
/// The counters are the run's running totals, not one round's delta, so a write lost to a
/// transient failure costs a refresh of the console rather than a permanent undercount.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn advance_full_sweep<'e, E: PgExecutor<'e>>(
    exec: E,
    claim: FullSweepClaim,
    rounds: i32,
    counters: FullSweepCounters,
) -> DbResult<()> {
    sqlx::query!(
        "UPDATE merge_full_sweep_state \
            SET heartbeat_at   = now(), \
                rounds         = $2, \
                pairs_examined = $3, \
                auto_merged    = $4, \
                queued         = $5, \
                requeued       = $6, \
                reopened       = $7, \
                withdrawn      = $8, \
                distinct_pairs = $9, \
                deferred       = $10, \
                blocked        = $11 \
          WHERE id AND claim_id = $1",
        claim.claim_id,
        rounds,
        counters.pairs_examined,
        counters.auto_merged,
        counters.queued,
        counters.requeued,
        counters.reopened,
        counters.withdrawn,
        counters.distinct,
        counters.deferred,
        counters.blocked,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Release the claim, recording why the run stopped.
///
/// Called however the run ends. A run that returned early on an error without reaching here
/// would leave the claim held until its lease expired, and the console showing a sweep that is
/// making no progress with nothing to say why.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn finish_full_sweep<'e, E: PgExecutor<'e>>(
    exec: E,
    claim: FullSweepClaim,
    stopped: &str,
    error: Option<&str>,
) -> DbResult<()> {
    sqlx::query!(
        "UPDATE merge_full_sweep_state \
            SET running      = false, \
                heartbeat_at = now(), \
                finished_at  = now(), \
                stopped      = $2, \
                error        = $3 \
          WHERE id AND claim_id = $1",
        claim.claim_id,
        stopped,
        error,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Read the exhaustive sweep's state, resolving `running` against the same [`LEASE_SECS`] the
/// claim is granted under.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; the row is created by migration 0056 and cannot be absent.
pub async fn read_full_sweep_state<'e, E: PgExecutor<'e>>(exec: E) -> DbResult<FullSweepState> {
    let row = sqlx::query!(
        "SELECT running \
                AND heartbeat_at IS NOT NULL \
                AND heartbeat_at >= now() - make_interval(secs => $1::double precision) \
                  AS \"running!\", \
                started_at, finished_at, rounds, pairs_examined, auto_merged, queued, requeued, \
                reopened, withdrawn, distinct_pairs, deferred, blocked, stopped, error \
         FROM merge_full_sweep_state WHERE id",
        LEASE_SECS,
    )
    .fetch_one(exec)
    .await?;
    Ok(FullSweepState {
        running: row.running,
        started_at: row.started_at,
        finished_at: row.finished_at,
        rounds: row.rounds,
        counters: FullSweepCounters {
            pairs_examined: row.pairs_examined,
            auto_merged: row.auto_merged,
            queued: row.queued,
            requeued: row.requeued,
            reopened: row.reopened,
            withdrawn: row.withdrawn,
            distinct: row.distinct_pairs,
            deferred: row.deferred,
            blocked: row.blocked,
        },
        stopped: row.stopped,
        error: row.error,
    })
}
