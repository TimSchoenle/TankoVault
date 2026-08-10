//! Reporting which stage a task is in, and accumulating where its wall clock went.
//!
//! One scan task does several distinguishable things — fetch a page, register its series, fan out
//! its children — and until now the only thing it published about any of them was that it was
//! still claimed. That is why a fast scan could sit at `0/1` for twenty minutes with nothing to
//! say about it. The reporter writes the current stage to the task row as it moves, and hands back
//! a [`StageTimings`] when the task ends so the settle can record where the time went.

use std::sync::Mutex;
use std::time::{Duration, Instant};
use tankovault_db::PgPool;
use tankovault_domain::{ScanStage, ScanTaskId, StageTimings};
use tankovault_fetch::FetchAccounting;

/// How often a stage that is only *progressing* rewrites the task row.
///
/// A stage change always writes. Progress inside one is throttled, because the fan-out reports
/// per chunk and the console reads at 2 s — a write per chunk would be a statement per thousand
/// series for a number nobody can see move.
const PROGRESS_WRITE_EVERY: Duration = Duration::from_secs(2);

/// The mutable half, behind one lock.
///
/// A `std::sync::Mutex` rather than `tokio`'s: nothing awaits while it is held — the guard is
/// dropped before the database write — so an async lock would buy nothing and make every
/// reporting call a suspension point.
struct State {
    /// The stage the task is in, and when it entered it.
    current: Option<(ScanStage, Instant)>,
    timings: StageTimings,
    /// When the row was last written for progress, for the throttle above.
    last_progress_write: Instant,
}

/// Reports a task's stage to its row and accumulates its per-stage timings.
///
/// The task id is optional: the one-shot CLI scan has no task row, and there the reporter still
/// accumulates timings (which nothing reads) rather than forcing every engine method to have two
/// versions.
pub(crate) struct StageReporter {
    pool: PgPool,
    task_id: Option<ScanTaskId>,
    state: Mutex<State>,
}

impl StageReporter {
    /// A reporter that writes to `task_id`'s row.
    pub(crate) fn for_task(pool: PgPool, task_id: ScanTaskId) -> Self {
        Self::new(pool, Some(task_id))
    }

    /// A reporter with no row to write to — the inline scan path.
    pub(crate) fn detached(pool: PgPool) -> Self {
        Self::new(pool, None)
    }

    fn new(pool: PgPool, task_id: Option<ScanTaskId>) -> Self {
        Self {
            pool,
            task_id,
            state: Mutex::new(State {
                current: None,
                timings: StageTimings::default(),
                // Backdated so the first progress report is never throttled. `checked_sub`
                // because a process started inside the window has no earlier instant to name,
                // and "now" there costs one throttled report rather than anything worse.
                last_progress_write: Instant::now()
                    .checked_sub(PROGRESS_WRITE_EVERY)
                    .unwrap_or_else(Instant::now),
            }),
        }
    }

    /// Enter `stage`, closing whatever stage was open and charging it its elapsed time.
    ///
    /// `detail` is what the stage is working against — a series path, a catalogue page — and is
    /// the field that turns "fetching chapters" into something an operator can act on.
    pub(crate) async fn enter(&self, stage: ScanStage, detail: Option<&str>) {
        {
            let mut held = self.lock();
            close_open_stage(&mut held);
            held.current = Some((stage, Instant::now()));
        }
        self.write(stage, None, detail).await;
    }

    /// Report progress within the current stage. Throttled; a caller may report per item.
    pub(crate) async fn progress(&self, done: usize, total: usize, detail: Option<&str>) {
        let current = {
            let mut held = self.lock();
            let Some((open, _)) = held.current else {
                return;
            };
            if held.last_progress_write.elapsed() < PROGRESS_WRITE_EVERY {
                return;
            }
            held.last_progress_write = Instant::now();
            open
        };
        self.write(current, Some((clamp(done), clamp(total))), detail)
            .await;
    }

    /// Close the task out and return where its time went, folding in what the fetch stack spent.
    ///
    /// Consuming, so a reporter cannot keep reporting after the settle it fed — a late stage write
    /// against a finished row is exactly the "completed run still fetching" the repository's
    /// `state = 'claimed'` guard also refuses.
    pub(crate) fn finish(self, fetched: FetchAccounting) -> StageTimings {
        let mut state = self
            .state
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        close_open_stage(&mut state);
        let mut timings = std::mem::take(&mut state.timings);
        timings.requests = fetched.requests;
        timings.fetch_ms = fetched.fetch_ms;
        timings.pace_wait_ms = fetched.pace_wait_ms;
        timings.solver_ms = fetched.solver_ms;
        timings.solver_calls = fetched.solver_calls;
        timings.throttled = fetched.throttled;
        timings
    }

    /// Recover the lock rather than propagate a poisoning: a panic in one stage must not stop the
    /// remaining stages of the task from being reported.
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Push the current stage to the task row. Best-effort by design — a scan must not fail
    /// because its progress report did not land.
    async fn write(&self, stage: ScanStage, progress: Option<(i32, i32)>, detail: Option<&str>) {
        let Some(task_id) = self.task_id else {
            return;
        };
        if let Err(e) =
            tankovault_db::repo::scans::set_task_stage(&self.pool, task_id, stage, progress, detail)
                .await
        {
            tracing::debug!(%task_id, %stage, error = %e, "could not record the task stage");
        }
    }
}

/// Charge the open stage its elapsed time and leave none open.
fn close_open_stage(state: &mut State) {
    if let Some((stage, since)) = state.current.take() {
        let millis = i64::try_from(since.elapsed().as_millis()).unwrap_or(i64::MAX);
        state.timings.add_stage(stage, millis);
    }
}

/// A count as the `int` column takes it. A catalogue page can carry 20k entries, so this is not
/// hypothetical at the low end and saturating is the right answer at the high one.
fn clamp(value: usize) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A detached reporter still has to accumulate: the timings it returns are what the settle
    /// records, and a reporter that only worked when it had a row to write to would leave every
    /// task's breakdown empty if the stage write ever became optional.
    #[tokio::test]
    async fn a_detached_reporter_still_charges_each_stage() {
        let reporter = StageReporter::detached(PgPool::connect_lazy("postgres://unused").unwrap());
        reporter
            .enter(ScanStage::SeriesMetadata, Some("/manga/x"))
            .await;
        tokio::time::sleep(Duration::from_millis(12)).await;
        reporter.enter(ScanStage::SeriesIngest, None).await;

        let timings = reporter.finish(FetchAccounting::default());
        assert!(
            timings.stages.contains_key("series_metadata"),
            "the stage the reporter left must be charged, not dropped"
        );
        assert!(
            timings.stages.contains_key("series_ingest"),
            "the stage still open at finish must be closed and charged"
        );
    }

    /// The fetch accounting is the half that explains the wall clock, and it arrives from a
    /// different mechanism (a task-local in the fetch stack) than the stages do. A `finish` that
    /// dropped it would leave a breakdown that says which stage was slow and never why.
    #[tokio::test]
    async fn finishing_folds_in_what_the_fetch_stack_spent() {
        let reporter = StageReporter::detached(PgPool::connect_lazy("postgres://unused").unwrap());
        let timings = reporter.finish(FetchAccounting {
            requests: 7,
            fetch_ms: 900,
            pace_wait_ms: 61_000,
            solver_ms: 2_000,
            solver_calls: 1,
            throttled: 2,
        });
        assert_eq!(timings.requests, 7);
        assert_eq!(timings.pace_wait_ms, 61_000);
        assert_eq!(timings.throttled, 2);
    }

    /// Progress reporting is called per fan-out chunk — potentially per thousand series — and each
    /// call would otherwise be a statement. The throttle is what makes it safe to report often.
    #[tokio::test]
    async fn progress_within_one_stage_is_throttled() {
        let reporter = StageReporter::detached(PgPool::connect_lazy("postgres://unused").unwrap());
        reporter.enter(ScanStage::CatalogFanout, None).await;
        // The first report always passes (the constructor backdates the clock); the second,
        // immediately after, must not.
        reporter.progress(1, 100, None).await;
        let throttled_at = reporter.lock().last_progress_write;
        reporter.progress(2, 100, None).await;
        assert_eq!(
            reporter.lock().last_progress_write,
            throttled_at,
            "a second report inside the window must not rewrite the row"
        );
    }
}
