//! Owns per-unit-of-work accounting for provider traffic: how many requests, how long they took,
//! and **how much of the wall clock went to waiting for permission to send one**.
//!
//! ## Why a task-local
//!
//! The figure that matters is per scan task, and the code that knows it is the rate limiter,
//! four decorator layers down and behind the `dyn Fetcher` every adapter is written against.
//! Threading an accumulator through would change the adapter trait, a public surface with a
//! dozen implementations, to carry a diagnostic.
//!
//! A `tokio::task_local` reaches the same place with no signature changes. It is sound because
//! the worker runs each scan task as its own spawned tokio task, so a scope entered there covers
//! exactly the futures that task awaits. Outside a scope, [`record`] is a no-op, so the one-shot
//! CLI path and the tests need not opt in.

use std::cell::Cell;
use std::future::Future;
use std::time::Duration;

/// Names the figure [`record`] adds to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Metered {
    /// A completed request, and the time inside it.
    Request(Duration),
    /// Time held back before sending, by the concurrency gate, the token rate, the crawl delay
    /// or the adaptive throttle penalty.
    PaceWait(Duration),
    /// A challenge solve, and the time it took.
    Solve(Duration),
    /// A response carrying a throttling status.
    Throttled,
}

/// What one unit of work spent on provider traffic.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FetchAccounting {
    /// The requests the base layer sent, failures included: time spent on requests that timed
    /// out was still spent.
    pub requests: i64,
    /// The milliseconds inside those requests, pacing excluded.
    pub fetch_ms: i64,
    /// The milliseconds held back before sending, by every pacing layer together.
    pub pace_wait_ms: i64,
    /// The milliseconds spent inside the challenge solver.
    pub solver_ms: i64,
    /// The solve attempts, successful or not.
    pub solver_calls: i64,
    /// The responses that carried a throttling status.
    pub throttled: i64,
}

/// Holds the live figures in `Cell`s, not atomics.
///
/// A task-local is reachable only from the task that entered the scope, and a tokio task is never
/// polled from two threads at once, so there is nothing to synchronise. `Cell` keeps [`record`]
/// to a load and a store on a per-request path.
#[derive(Default)]
struct Meter {
    requests: Cell<i64>,
    fetch_ms: Cell<i64>,
    pace_wait_ms: Cell<i64>,
    solver_ms: Cell<i64>,
    solver_calls: Cell<i64>,
    throttled: Cell<i64>,
}

tokio::task_local! {
    static METER: Meter;
}

/// Converts to milliseconds, saturating so an absurd duration reads as large, not negative.
#[expect(
    clippy::cast_possible_truncation,
    reason = "the min() below bounds the value inside i64 before the cast"
)]
fn millis(d: Duration) -> i64 {
    d.as_millis().min(i64::MAX as u128) as i64
}

/// Adds one figure to the enclosing [`measured`] scope.
///
/// A no-op outside one, so instrumenting a fetch layer never obliges its callers to opt in.
pub fn record(what: Metered) {
    let _ = METER.try_with(|meter| {
        let bump = |cell: &Cell<i64>, by: i64| cell.set(cell.get().saturating_add(by));
        match what {
            Metered::Request(elapsed) => {
                bump(&meter.requests, 1);
                bump(&meter.fetch_ms, millis(elapsed));
            }
            Metered::PaceWait(waited) => bump(&meter.pace_wait_ms, millis(waited)),
            Metered::Solve(elapsed) => {
                bump(&meter.solver_calls, 1);
                bump(&meter.solver_ms, millis(elapsed));
            }
            Metered::Throttled => bump(&meter.throttled, 1),
        }
    });
}

/// Runs `work` in a fresh accounting scope, returning its output and what its provider traffic
/// cost.
///
/// Scopes do not nest usefully: an inner one shadows the outer, which then under-counts. Enter
/// one per reported unit of work; for the worker, that is one scan task.
pub async fn measured<F: Future>(work: F) -> (F::Output, FetchAccounting) {
    METER
        .scope(Meter::default(), async move {
            let output = work.await;
            let totals = METER.with(|meter| FetchAccounting {
                requests: meter.requests.get(),
                fetch_ms: meter.fetch_ms.get(),
                pace_wait_ms: meter.pace_wait_ms.get(),
                solver_ms: meter.solver_ms.get(),
                solver_calls: meter.solver_calls.get(),
                throttled: meter.throttled.get(),
            });
            (output, totals)
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_scope_totals_what_was_recorded_inside_it() {
        let ((), totals) = measured(async {
            record(Metered::Request(Duration::from_millis(120)));
            record(Metered::Request(Duration::from_millis(80)));
            record(Metered::PaceWait(Duration::from_secs(3)));
            record(Metered::Solve(Duration::from_millis(1_500)));
            record(Metered::Throttled);
        })
        .await;

        assert_eq!(totals.requests, 2);
        assert_eq!(totals.fetch_ms, 200);
        assert_eq!(totals.pace_wait_ms, 3_000);
        assert_eq!(totals.solver_calls, 1);
        assert_eq!(totals.solver_ms, 1_500);
        assert_eq!(totals.throttled, 1);
    }

    /// Recording outside a scope is silent, not a panic.
    ///
    /// Every fetch layer records unconditionally, and the CLI scan and most tests open no scope.
    /// A `with` in place of `try_with` would abort the process there.
    #[tokio::test]
    async fn recording_without_a_scope_does_nothing() {
        record(Metered::Request(Duration::from_millis(5)));
        let ((), totals) = measured(async {}).await;
        assert_eq!(totals, FetchAccounting::default());
    }

    /// A scope covers only the futures its own task awaits, which lets the worker attribute
    /// traffic to one scan task while several run concurrently.
    #[tokio::test]
    async fn concurrent_scopes_do_not_leak_into_each_other() {
        let one = tokio::spawn(measured(async {
            record(Metered::Request(Duration::from_millis(10)));
            tokio::task::yield_now().await;
            record(Metered::Request(Duration::from_millis(10)));
        }));
        let two = tokio::spawn(measured(async {
            record(Metered::Request(Duration::from_millis(70)));
        }));

        let ((), first) = one.await.expect("the first scope finishes");
        let ((), second) = two.await.expect("the second scope finishes");
        assert_eq!((first.requests, first.fetch_ms), (2, 20));
        assert_eq!((second.requests, second.fetch_ms), (1, 70));
    }
}
