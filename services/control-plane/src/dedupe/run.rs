//! Owns one sweep from start to finish: its budget and scope, its passes, and the exhaustive run.

use std::collections::HashSet;

use tankovault_contracts::admin::MergeSweepView;
use tankovault_db::PgPool;
use tankovault_db::repo::matching::{self, DuplicatePair};
use tankovault_domain::UserId;
use tankovault_matcher::Thresholds;
use tracing::Instrument as _;

use super::{orphans::clean_up_orphans, pass::sweep_once};

/// How much work one sweep may do. `max_auto_merges` bounds a background action that deletes
/// rows: without it, a bad threshold change could collapse the whole catalogue in one run.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SweepBudget {
    /// Newly-blocked pairs to shortlist: ones the sweep has never recorded a verdict for.
    pub pairs: i64,
    /// Open queue rows to re-score, oldest-refreshed first.
    pub requeue: i64,
    /// Pairs already judged distinct to reconsider, oldest-refreshed first.
    pub recheck: i64,
    /// Automatic merges permitted in this run.
    pub max_auto_merges: i64,
}

/// Which shortlists one sweep draws from.
///
/// Discovery blocks the whole catalogue on the compact title key, so it costs the same whether
/// anything changed or not; the two rotations are index scans sized by their own budgets.
/// Separating them is what lets the queue be re-scored on a cadence discovery could not afford:
/// see `scheduler.merge_sweep_rotation_interval_secs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SweepScope {
    /// All three shortlists, including pairs never judged before.
    Full,
    /// The requeue and recheck rotations only.
    Rotation,
}

/// The most passes one sweep will make.
///
/// A pass beyond the first runs only when the one before it skipped a pair because of its own
/// merge, so this bounds the work a pathological chain can ask for. It does **not** bound the
/// damage: every pass is charged against the same [`SweepBudget::max_auto_merges`], so a sweep
/// deletes no more rows for having taken three passes than it would have in one.
const MAX_PASSES: u32 = 3;

/// Runs a sweep, following the merge chains it creates.
///
/// `actor` is the operator who triggered it, or `None` for the scheduled run, recorded
/// in `merge_candidates.resolved_by` and on every decision this run journals.
///
/// # Why more than one pass
///
/// A pass that merges A into B has invalidated the facts it loaded for every other pair naming
/// A, so it skips them (`chains_deferred`). Left there, a three-way duplicate needs three
/// scheduler ticks (an hour apart each) to collapse, and for the whole of that time the
/// catalogue holds a duplicate the sweep has already proved. Re-running immediately costs
/// another shortlist query and resolves it at once.
///
/// # Errors
///
/// Returns an error on a database failure; merges already committed stay committed (idempotent,
/// safe to re-run).
pub(crate) async fn sweep(
    pool: &PgPool,
    policy: Thresholds,
    budget: SweepBudget,
    scope: SweepScope,
    actor: Option<UserId>,
) -> anyhow::Result<SweepRun> {
    let mut remaining = budget;
    let mut total = MergeSweepView::default();
    let mut shortlisted = Vec::new();
    for pass in 0..MAX_PASSES {
        let (report, pairs) = sweep_once(pool, policy, remaining, scope, actor).await?;
        accumulate(&mut total, &report);
        shortlisted.extend(pairs);
        // Spent against the run, not the pass: the ceiling is what stands between a bad
        // threshold and a collapsed catalogue, and it has to mean the same thing however many
        // passes the chains ask for.
        remaining.max_auto_merges -= report.auto_merged;
        if report.chains_deferred == 0 || remaining.max_auto_merges <= 0 {
            break;
        }
        tracing::debug!(
            pass = pass + 1,
            chains = report.chains_deferred,
            "re-running the sweep for a merge chain"
        );
    }
    Ok(SweepRun {
        report: total,
        shortlisted,
    })
}

/// What one call to [`sweep`] did, and which pairs it looked at doing it.
pub(crate) struct SweepRun {
    pub(crate) report: MergeSweepView,
    /// Every pair the run's passes shortlisted. May repeat a pair across passes.
    ///
    /// Only [`run_full_sweep`] reads it, and only to answer "is there more to do?". The counts
    /// cannot answer that: two of the three shortlists are ordered least-recently-scored first
    /// and re-scoring moves a pair to the back of that order, so they keep returning rows
    /// forever. Whether a round shortlisted anything *new* is the question that terminates.
    pub(crate) shortlisted: Vec<DuplicatePair>,
}

/// Adds one pass's counts into the run's totals.
///
/// Everything sums except `chains_deferred`, which is a statement about what is *left* to do and
/// so belongs to the last pass alone; summing it would report chains that later passes resolved.
pub(super) fn accumulate(total: &mut MergeSweepView, pass: &MergeSweepView) {
    total.pairs_examined += pass.pairs_examined;
    total.auto_merged += pass.auto_merged;
    total.queued += pass.queued;
    total.requeued += pass.requeued;
    total.reopened += pass.reopened;
    total.withdrawn += pass.withdrawn;
    total.distinct += pass.distinct;
    total.deferred += pass.deferred;
    total.blocked += pass.blocked;
    total.chains_deferred = pass.chains_deferred;
}

/// The budget an exhaustive run draws under: the scheduled one with its ceiling lifted.
///
/// `max_auto_merges` bounds a *background* action: it is the rate at which a mistaken threshold
/// can delete series between two looks by a human. An exhaustive run is that look: an operator
/// pressed a button that says it sweeps the whole catalogue, and a run which stopped two hundred
/// merges in reported work remaining and asked to be pressed again. `scanning.auto_merge` is the
/// switch that decides whether this may merge at all; within it, the walk runs to the end.
///
/// The three shortlist sizes stay as configured. They are the page the walk is drawn in, not a
/// bound on it: rounds continue until one comes back with nothing new, so a smaller page costs
/// rounds, not coverage, and it is what keeps one round's facts and scoring in memory.
pub(super) const fn exhaustive_budget(scheduled: SweepBudget) -> SweepBudget {
    SweepBudget {
        max_auto_merges: i64::MAX,
        ..scheduled
    }
}

/// Decides when an exhaustive run has nothing left to do.
///
/// A run cannot stop when a round examines nothing, because two of the three shortlists never
/// come back empty: `open_merge_pairs` and `distinct_merge_pairs` are ordered
/// least-recently-scored first, and re-scoring a pair moves it to the back of its own order. A
/// catalogue holding one open candidate would hand that same pair back for ever.
/// What ends the walk is a round that shortlists nothing *new*, which is exactly one full
/// rotation of each list.
///
/// That is the only thing that ends it. The walk terminates because the catalogue is finite and
/// merging only ever shrinks it: every round either judges a pair no round has judged or is the
/// last. The costs that grow with it are [`Self::seen`] (two ids per distinct pair) and the
/// wall clock, and a run holds its claim by stamping a heartbeat every round, so one that stops
/// making progress is visible in the console instead of silent.
pub(super) struct RoundLoop {
    /// Every pair shortlisted so far this run.
    seen: HashSet<DuplicatePair>,
    rounds: i32,
}

impl RoundLoop {
    pub(super) fn new() -> Self {
        Self {
            seen: HashSet::new(),
            rounds: 0,
        }
    }

    /// Fold one round in, answering whether the walk is finished.
    pub(super) fn admit(&mut self, run: &SweepRun) -> bool {
        self.rounds = self.rounds.saturating_add(1);
        // `count()`, not `any()`: every pair has to reach `seen`, or one the short-circuit
        // skipped would read as new next round and the walk would never end.
        let fresh = run
            .shortlisted
            .iter()
            .filter(|pair| self.seen.insert(**pair))
            .count();
        fresh == 0
    }
}

/// The run's totals in the shape the state row stores.
///
/// `chains_deferred` is dropped: it says the last *pass* skipped a pair because of its own merge,
/// and a run that draws rounds until the shortlists hold nothing new has already come back for it.
const fn counters(total: &MergeSweepView) -> matching::FullSweepCounters {
    matching::FullSweepCounters {
        pairs_examined: total.pairs_examined,
        auto_merged: total.auto_merged,
        queued: total.queued,
        requeued: total.requeued,
        reopened: total.reopened,
        withdrawn: total.withdrawn,
        distinct: total.distinct,
        deferred: total.deferred,
        blocked: total.blocked,
    }
}

/// Starts an exhaustive sweep on a task of its own, answering whether this call started one.
///
/// `false` means a live run already holds the claim. The correct response is to say so, not to
/// queue behind it: the other run is doing this one's work.
///
/// **An exhaustive run must never be awaited inside a request handler.** It draws rounds until
/// the shortlists hold nothing new, which is minutes, and the API's upstream timeout is
/// twenty-five seconds: a handler awaiting one would be dropped mid-run, taking the only call
/// that releases the claim with it. The lease on the claim is the second half of that guard;
/// this is the first. `recsys::build_detached` carries the same note because that is where the
/// failure was found the first time.
///
/// # Errors
///
/// Returns an error on a database failure while claiming. Failures *within* the run are recorded on
/// `merge_full_sweep_state`, which is where the console reads them from.
pub(crate) async fn sweep_all_detached(
    pool: &PgPool,
    policy: Thresholds,
    budget: SweepBudget,
    actor: Option<UserId>,
) -> anyhow::Result<bool> {
    let Some(claim) = matching::claim_full_sweep(pool).await? else {
        tracing::debug!("exhaustive duplicate sweep already running; not starting another");
        return Ok(false);
    };

    let pool = pool.clone();
    let budget = exhaustive_budget(budget);
    let run = async move {
        let outcome = run_full_sweep(&pool, policy, budget, actor, claim).await;
        // The two values `merge_full_sweep_state.stopped` can hold. A run either walks the
        // shortlists out or fails trying; there is no longer a limit it can stop at.
        let (stopped, error) = match &outcome {
            Ok(()) => ("exhausted", None),
            Err(error) => ("failed", Some(error.to_string())),
        };
        // Released however the run ended. A run that returned early without reaching here would
        // hold the claim until its lease expired, with the console showing a sweep making no
        // progress and nothing saying why.
        if let Err(e) = matching::finish_full_sweep(&pool, claim, stopped, error.as_deref()).await {
            tracing::warn!(error = %e, "could not release the exhaustive duplicate sweep's claim");
        }
        match outcome {
            Ok(()) => tracing::info!("exhaustive duplicate sweep complete"),
            Err(error) => tracing::warn!(error = %error, "exhaustive duplicate sweep failed"),
        }
    };
    // Detached but kept in the trace, and named the same as the scheduled sweep so an
    // operator-triggered run and a timer-driven one aggregate as one operation.
    tokio::spawn(tankovault_service::in_current_trace(run.instrument(
        tracing::info_span!("merge_sweep", "sentry.op" = "cron", scope = "exhaustive"),
    )));
    Ok(true)
}

/// Draw rounds under a claim already taken, until one turns up nothing new.
///
/// # Errors
///
/// Returns an error on a database failure. Whatever earlier rounds committed stays committed; every
/// round is an ordinary sweep and re-running is idempotent.
async fn run_full_sweep(
    pool: &PgPool,
    policy: Thresholds,
    budget: SweepBudget,
    actor: Option<UserId>,
    claim: matching::FullSweepClaim,
) -> anyhow::Result<()> {
    let mut rounds = RoundLoop::new();
    let mut total = MergeSweepView::default();
    total.auto_merged += clean_up_orphans(pool, policy, actor).await?;
    matching::advance_full_sweep(pool, claim, rounds.rounds, counters(&total)).await?;
    loop {
        let run = sweep(pool, policy, budget, SweepScope::Full, actor).await?;
        accumulate(&mut total, &run.report);
        let done = rounds.admit(&run);
        // Written after every round, including the last: this row is the only progress an
        // operator can see, and a run that only reported at the end would be indistinguishable
        // from one that hung on its first.
        matching::advance_full_sweep(pool, claim, rounds.rounds, counters(&total)).await?;
        if done {
            return Ok(());
        }
    }
}
