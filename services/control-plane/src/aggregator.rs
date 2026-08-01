//! Progress aggregator: finalises a scan run once every planned task has settled, then
//! republishes one terminal progress event for the console SSE. Finalisation is a single
//! atomic UPDATE ([`tankovault_db::repo::scans::finalize_if_complete`]) that fires exactly
//! once, so a redelivered event is a no-op that cannot loop.

use std::time::Duration;
use tankovault_bus::Bus;
use tankovault_contracts::{ProgressEvent, subjects};
use tankovault_db::PgPool;
use time::OffsetDateTime;

/// Consume `scan.progress` until `shutdown`, finalising runs as their tasks settle.
///
/// Must use the shared consume loop, not a bare loop — without a cancellation arm this
/// hangs or gets killed mid-message on `SIGTERM`.
pub(crate) async fn run(
    pool: PgPool,
    bus: Bus,
    shutdown: tokio_util::sync::CancellationToken,
) -> anyhow::Result<()> {
    let consumer = bus
        .event_consumer(subjects::PROGRESS_CONSUMER, subjects::PROGRESS_SUBJECT)
        .await?;

    // Idempotent (`finalize_if_complete` fires once) and self-healing: the next task to
    // settle on this run re-triggers finalisation. Giving up after 3 retries is safe here,
    // unlike for the notifier.
    let policy = tankovault_bus::ConsumePolicy {
        max_deliveries: 3,
        backoff: |_| Duration::from_secs(15),
        heartbeat: None,
    };

    tankovault_bus::consume(
        consumer,
        shutdown,
        policy,
        subjects::PROGRESS_SUBJECT,
        move |event: ProgressEvent, _msg| {
            let pool = pool.clone();
            let bus = bus.clone();
            async move {
                handle_progress(&pool, &bus, &event).await?;
                Ok(tankovault_bus::Disposition::Ack)
            }
        },
    )
    .await?;
    Ok(())
}

/// Finalise the run behind `event` when every task has settled, then relay one terminal
/// progress event. The event counters are a cheap gate; the DB UPDATE is authoritative.
async fn handle_progress(pool: &PgPool, bus: &Bus, event: &ProgressEvent) -> anyhow::Result<()> {
    if !should_finalize(event.total_tasks, event.done_tasks, event.failed_tasks) {
        return Ok(());
    }
    if let Some(run) = tankovault_db::repo::scans::finalize_if_complete(pool, event.run_id).await? {
        tracing::info!(
            run_id = %run.id,
            state = ?run.state,
            done = run.done_tasks,
            failed = run.failed_tasks,
            "scan run finalised"
        );
        let terminal = ProgressEvent {
            run_id: run.id,
            provider_id: run.provider_id,
            mode: run.mode,
            state: run.state,
            total_tasks: run.total_tasks,
            done_tasks: run.done_tasks,
            failed_tasks: run.failed_tasks,
            at: OffsetDateTime::now_utc(),
        };
        bus.publish_progress(&terminal).await?;
    }
    Ok(())
}

/// Cheap, DB-free mirror of the SQL finalisation guard; used to skip the atomic UPDATE
/// for the common still-in-flight event.
pub(crate) fn should_finalize(total_tasks: i32, done_tasks: i32, failed_tasks: i32) -> bool {
    total_tasks > 0 && done_tasks.saturating_add(failed_tasks) >= total_tasks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_finalised_while_tasks_outstanding() {
        assert!(!should_finalize(3, 1, 1));
        assert!(!should_finalize(1, 0, 0));
    }

    #[test]
    fn not_finalised_without_any_planned_tasks() {
        // A run that has not yet had tasks fanned out must not be finalised early.
        assert!(!should_finalize(0, 0, 0));
    }

    #[test]
    fn finalised_when_all_tasks_settled() {
        assert!(should_finalize(1, 1, 0)); // all done
        assert!(should_finalize(2, 0, 2)); // all failed
        assert!(should_finalize(4, 3, 1)); // mixed
        assert!(should_finalize(2, 3, 0)); // over-count (idempotent republish) still settles
    }
}
