//! Progress aggregator (design §12).
//!
//! Consumes the compact [`ProgressEvent`]s that workers publish on
//! [`subjects::PROGRESS_SUBJECT`] after every task settles. Its job is to **finalise a
//! run**: once every planned task of a run has completed or failed, it transitions the
//! `scan_runs` row to its terminal state and republishes one final progress event so the
//! console SSE sees a `completed`/`failed` run without DB-polling.
//!
//! Run-counter aggregation itself already happens DB-side in
//! [`tankovault_db::repo::scans::complete_task`]/`fail_task`/`skip_task`; this consumer only
//! adds the *completion* transition (which nothing else performed before) and the NATS
//! relay. The finalisation is a single atomic UPDATE
//! ([`tankovault_db::repo::scans::finalize_if_complete`]) that fires exactly once, so the
//! republished terminal event — re-consumed here — is a no-op and cannot loop.

use std::time::Duration;
use tankovault_bus::Bus;
use tankovault_contracts::{ProgressEvent, subjects};
use tankovault_db::PgPool;
use time::OffsetDateTime;

/// Consume `scan.progress` until `shutdown`, finalising runs as their tasks settle.
///
/// Previously a bare `while let` with **no cancellation arm**, so this consumer could not be
/// drained on `SIGTERM` — the control-plane's graceful shutdown hung on it or killed it
/// mid-message. The shared loop supplies the shutdown arm along with the delivery semantics.
pub(crate) async fn run(
    pool: PgPool,
    bus: Bus,
    shutdown: tokio_util::sync::CancellationToken,
) -> anyhow::Result<()> {
    let consumer = bus
        .event_consumer(subjects::PROGRESS_CONSUMER, subjects::PROGRESS_SUBJECT)
        .await?;

    // Retries a couple of times, then settles. Finalisation is a single idempotent UPDATE
    // (`finalize_if_complete`), so a redelivery is a no-op — but it is also self-healing:
    // the *next* task to settle on the same run republishes progress and finalises it then.
    // That is why giving up here is safe in a way it is not for the notifier.
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

/// Cheap, DB-free predicate mirroring the SQL finalisation guard: a run is a candidate
/// for finalisation once it has at least one planned task and every task has settled
/// (`done + failed >= total`). Used to skip the atomic UPDATE for the common
/// still-in-flight event.
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
