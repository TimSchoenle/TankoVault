//! Repairs the drift between `JetStream` (the truth for dispatch) and `scan_tasks` (the truth
//! for progress).
//!
//! Nothing else closes this gap. A task row is committed and then its message is published, and
//! every way that pair can come apart — a publish that never landed, a stream purged or
//! recreated under a running deployment, a worker that acked and then died, a queue that stopped
//! consuming — leaves a row that is open forever and a run that can never settle. From the
//! database's side there is no failure to see, so no retry is triggered and nothing is logged;
//! the run simply reads `RUNNING` with an unmoving counter, and until it goes stale it also
//! suppresses the provider's next run of the same mode. That is the shape of every "the scan
//! queue is stuck" report.
//!
//! The repair is evidence-based rather than timed, which is the part that makes it safe to run
//! unattended: a lane's open rows are compared against the message count the broker actually
//! holds for it, and only the difference is republished. Age alone would be a bad signal — a
//! catalogue scan legitimately leaves tens of thousands of tasks queued for hours behind its
//! provider's one slot, and republishing those would fan the backlog out a second time.

use std::time::Duration;
use tankovault_contracts::{ScanTaskMessage, TaskKind};
use tankovault_db::repo::scans::{OpenLane, StrandedTask};
use tankovault_domain::ScanMode;
use time::OffsetDateTime;

use crate::AppState;

/// How long a row must have been open before a missing message is read as lost rather than as a
/// publish still in flight.
///
/// The window it has to cover is the gap between the task row's commit and its broker ack, which
/// is milliseconds. Minutes of margin costs nothing — the tasks this repairs have been stranded
/// for as long as the deployment has been broken — and buys immunity to a slow publish, a
/// paused replica and a clock that is not quite in step.
const AGED_AFTER: Duration = Duration::from_secs(300);

/// Ceiling on republishes per lane per pass.
///
/// A deficit this large is not a lost publish, it is a stream that was emptied or replaced —
/// and in that case the whole backlog is missing. Republishing it in bounded slices keeps one
/// pass from turning into an hours-long fan-out that blocks the scheduler, and the next pass
/// resumes from what is still open.
const MAX_REPUBLISH_PER_LANE: i64 = 500;

/// Ceiling on runs closed per pass, for the same reason.
const MAX_RUNS_PER_PASS: i64 = 500;

/// What one pass repaired.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Repairs {
    /// Task messages republished because the broker no longer held one.
    pub(crate) republished: u64,
    /// Runs finalised whose tasks had all settled.
    pub(crate) finalised: u64,
    /// Runs failed that never planned a task at all.
    pub(crate) abandoned: u64,
    /// Lanes skipped because the broker could not be asked about them.
    pub(crate) unreadable_lanes: u64,
}

impl Repairs {
    /// Whether this pass changed anything, so a quiet reconciler stays quiet in the log.
    fn is_empty(&self) -> bool {
        self.republished == 0 && self.finalised == 0 && self.abandoned == 0
    }
}

/// Run one reconciliation pass.
///
/// Every repair is idempotent, so a pass that overlaps a worker or another replica is wasteful
/// rather than wrong: a republished task whose message did still exist is claimed once and
/// declined on the second delivery, and finalisation is a single conditional UPDATE.
pub(crate) async fn pass(state: &AppState) -> anyhow::Result<Repairs> {
    let mut repairs = Repairs::default();

    for lane in tankovault_db::repo::scans::open_task_lanes(&state.pool, AGED_AFTER).await? {
        match repair_lane(state, &lane).await {
            Ok(republished) => repairs.republished += republished,
            Err(e) => {
                // One unreadable lane must not abandon the others: a lane is a provider, and
                // the reason this one cannot be read (an invalid slug, a broker blip) has
                // nothing to do with the rest.
                tracing::warn!(
                    provider = %lane.provider_slug,
                    scan = %lane.mode,
                    error = %e,
                    "could not reconcile a provider lane; leaving it for the next pass"
                );
                repairs.unreadable_lanes += 1;
            }
        }
    }

    repairs.finalised = finalise_settled_runs(state).await?;
    repairs.abandoned = fail_abandoned_runs(state).await?;

    if !repairs.is_empty() {
        tracing::warn!(
            republished = repairs.republished,
            finalised = repairs.finalised,
            abandoned = repairs.abandoned,
            "repaired scan dispatch that had drifted from the database"
        );
    }
    Ok(repairs)
}

/// Republish one lane's stranded tasks, and return how many.
///
/// The comparison is deliberately conservative in both directions. `open_tasks` counts every
/// open row while the deficit is charged only against the *aged* ones, so a task published
/// seconds ago can absorb the difference and keep a fresh row from being republished. And a lane
/// the broker cannot account for at all — no consumer yet, because no worker has ever opened it
/// — is skipped rather than treated as empty.
async fn repair_lane(state: &AppState, lane: &OpenLane) -> anyhow::Result<u64> {
    let Some(backlog) = state
        .bus
        .lane_backlog(&lane.provider_slug, lane.mode)
        .await?
    else {
        tracing::debug!(
            provider = %lane.provider_slug,
            scan = %lane.mode,
            "no task lane on the broker yet; nothing to reconcile against"
        );
        return Ok(0);
    };

    let missing = deficit(lane, backlog).min(MAX_REPUBLISH_PER_LANE);
    if missing <= 0 {
        return Ok(0);
    }

    let stranded = tankovault_db::repo::scans::stranded_tasks(
        &state.pool,
        lane.provider_id,
        lane.mode,
        AGED_AFTER,
        missing,
    )
    .await?;

    tracing::warn!(
        provider = %lane.provider_slug,
        scan = %lane.mode,
        open_tasks = lane.open_tasks,
        broker_backlog = backlog,
        republishing = stranded.len(),
        "the broker holds fewer messages than this lane has open tasks; republishing the oldest"
    );

    let mut republished = 0;
    for task in stranded {
        if republish(state, lane, &task).await? {
            republished += 1;
        }
    }
    Ok(republished)
}

/// How many of a lane's open tasks the broker is not holding a message for.
///
/// Split out to be tested without a broker: it is the whole of the decision to republish, and
/// getting it wrong is silent either way — too eager duplicates a catalogue fan-out, too shy
/// leaves the queue as stuck as it was.
fn deficit(lane: &OpenLane, backlog: u64) -> i64 {
    let backlog = i64::try_from(backlog).unwrap_or(i64::MAX);
    // Charged against the aged rows only, but netted against the *whole* lane's messages: a
    // message the broker holds might belong to any of the open rows, including a fresh one, and
    // assuming otherwise is what would republish a task that is merely young.
    lane.aged_tasks.saturating_sub(backlog).max(0)
}

/// Rebuild one task's message and publish it. `false` when the row cannot be turned back into a
/// message, which is a corrupt row rather than a drift and is not this reconciler's to fix.
async fn republish(state: &AppState, lane: &OpenLane, task: &StrandedTask) -> anyhow::Result<bool> {
    let Some(kind) = TaskKind::from_token(&task.kind) else {
        tracing::warn!(
            task_id = %task.id,
            kind = %task.kind,
            "cannot republish a scan task whose kind is not one this build knows"
        );
        return Ok(false);
    };
    state
        .bus
        .publish_task(&ScanTaskMessage {
            task_id: task.id,
            run_id: task.run_id,
            provider_id: lane.provider_id,
            provider_slug: lane.provider_slug.clone(),
            mode: lane.mode,
            kind,
            target: task.target.clone(),
            traceparent: None,
        })
        .await?;
    repaired(&lane.provider_slug, lane.mode, "republished");
    Ok(true)
}

/// Close runs whose tasks have all settled, emitting the terminal progress event the lost one
/// would have carried so the console and the aggregator see the same ending.
async fn finalise_settled_runs(state: &AppState) -> anyhow::Result<u64> {
    let mut finalised = 0;
    for run_id in
        tankovault_db::repo::scans::runs_awaiting_finalisation(&state.pool, MAX_RUNS_PER_PASS)
            .await?
    {
        let Some(run) =
            tankovault_db::repo::scans::finalize_if_complete(&state.pool, run_id).await?
        else {
            // Another actor got there first, which is the ordinary race with the aggregator.
            continue;
        };
        tracing::info!(
            run_id = %run.id,
            state = ?run.state,
            done = run.done_tasks,
            failed = run.failed_tasks,
            "finalised a scan run whose terminal progress event was lost"
        );
        state
            .bus
            .publish_progress(&tankovault_contracts::ProgressEvent {
                run_id: run.id,
                provider_id: run.provider_id,
                mode: run.mode,
                state: run.state,
                total_tasks: run.total_tasks,
                done_tasks: run.done_tasks,
                failed_tasks: run.failed_tasks,
                at: OffsetDateTime::now_utc(),
            })
            .await?;
        finalised += 1;
    }
    Ok(finalised)
}

/// Fail runs that never planned a task, so they stop suppressing the provider's next one.
async fn fail_abandoned_runs(state: &AppState) -> anyhow::Result<u64> {
    let failed =
        tankovault_db::repo::scans::fail_unplanned_runs(&state.pool, AGED_AFTER, MAX_RUNS_PER_PASS)
            .await?;
    for run_id in &failed {
        tracing::warn!(
            %run_id,
            "failed a scan run that was opened but never planned a task"
        );
    }
    Ok(failed.len() as u64)
}

/// Count one repair.
///
/// The counter an operator alerts on: a healthy deployment never repairs anything, so any
/// sustained rate here is dispatch losing messages — the cause, not the symptom the console
/// shows.
fn repaired(provider_slug: &str, mode: ScanMode, action: &'static str) {
    metrics::counter!(
        tankovault_service::metrics::names::SCAN_DISPATCH_REPAIRS,
        "provider" => provider_slug.to_owned(),
        "scan" => mode.as_str(),
        "action" => action,
    )
    .increment(1);
}

#[cfg(test)]
mod tests {
    use super::*;
    use tankovault_domain::ProviderId;

    fn lane(open_tasks: i64, aged_tasks: i64) -> OpenLane {
        OpenLane {
            provider_id: ProviderId::new(),
            provider_slug: "kunmanga".to_owned(),
            mode: ScanMode::Full,
            open_tasks,
            aged_tasks,
        }
    }

    #[test]
    fn a_lane_the_broker_still_holds_every_message_for_is_left_alone() {
        assert_eq!(deficit(&lane(3, 3), 3), 0);
        // More messages than rows: a settled row whose message has not been acked yet.
        assert_eq!(deficit(&lane(3, 3), 9), 0);
    }

    #[test]
    fn only_the_missing_messages_are_republished() {
        assert_eq!(deficit(&lane(5, 5), 2), 3);
        assert_eq!(deficit(&lane(1, 1), 0), 1);
    }

    /// A task whose message has not been published *yet* must not be republished.
    ///
    /// The row is committed before the publish, so every task passes through a moment of looking
    /// exactly like a stranded one. Charging the deficit against the aged rows alone is what
    /// makes that moment invisible here — and getting it wrong duplicates work silently rather
    /// than failing, which on a catalogue fan-out means a second pass over tens of thousands of
    /// series.
    #[test]
    fn a_task_younger_than_the_grace_period_is_never_republished() {
        // Ten open rows, none of them aged: whatever the broker holds, nothing is repaired.
        assert_eq!(deficit(&lane(10, 0), 0), 0);
        // Nine fresh rows and one aged one, with one message held: the aged row is covered.
        assert_eq!(deficit(&lane(10, 1), 1), 0);
    }

    /// The repair has to be quiet when there is nothing to repair.
    #[test]
    fn a_pass_that_changed_nothing_reports_nothing() {
        assert!(Repairs::default().is_empty());
        assert!(
            Repairs {
                unreadable_lanes: 4,
                ..Repairs::default()
            }
            .is_empty(),
            "a lane that could not be read is not a repair"
        );
        assert!(
            !Repairs {
                republished: 1,
                ..Repairs::default()
            }
            .is_empty()
        );
    }
}
