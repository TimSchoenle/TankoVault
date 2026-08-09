//! The scan run/task lifecycle (`crates/db/src/repo/scans.rs`, TEST F-05).
//!
//! # What is actually at stake here
//!
//! The task row is the truth for **progress and audit**; `JetStream` is the truth for dispatch
//! (design §2). That split means these eleven statements are the only record of how much of a scan
//! has happened, and delivery is **at-least-once** — a worker sees the same task twice whenever an
//! ack is lost or an ack deadline lapses. So every statement here has to be idempotent under
//! replay, and the interesting failures are all arithmetic rather than type errors:
//!
//! - **`done_tasks` / `failed_tasks` are counts of *tasks*, not of settle calls.**
//!   [`finalize_if_complete`] decides a run is over by comparing their sum against `total_tasks`,
//!   so counting one task twice finalises a run — and emits its single terminal event — while
//!   other tasks are still running. See `SCAN-1` below.
//! - **`ON CONFLICT DO NOTHING` is what makes fan-out replay-safe.** A redelivered
//!   `catalog_page` re-attempts every child insert; the conflict clause is the only reason that
//!   does not double the run's planned total.
//! - **`COALESCE(started_at, now())`** is the same property for the run: a re-`start` must not
//!   restamp it, or a run's duration resets every time the planner message is redelivered.
//!
//! None of that is visible to `sqlx prepare --check`, and none of it is visible to the worker's
//! own unit tests, which stop at the retry policy and never reach the database.
//!
//! # SCAN-1 — what this suite found
//!
//! `claim_task` had **no state guard at all**, and the three settle statements each excluded only
//! the state they were about to write. So the ordinary redelivery path —
//! `claim → complete → (ack lost) → claim → complete` — put a finished task back into `claimed`,
//! which re-opened `complete_task`'s `state <> 'done'` guard and counted the task a second time.
//! A `done` task could also be turned into a `failed` one, adding a `failed_tasks` count next to
//! its `done_tasks` one, and a `failed` task could be `skipped`, which `skip_task`'s guard did not
//! exclude. All four statements now exclude the same three terminal states: a task settles once.
//!
//! Opt-in: gated behind the `integration` feature because it requires Docker.
#![cfg(feature = "integration")]

use serde_json::{Value as Json, json};
use tankovault_db::DbError;
use tankovault_db::repo::providers::{self};
use tankovault_db::repo::scans;
use tankovault_domain::{ProviderId, RunState, ScanMode, ScanRunId, ScanTaskId, TaskState};
use tankovault_test_support::{TestDb, seed};

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

/// A run with `planned` tasks declared, in the `running` state — the state every task settles
/// into and the only state [`scans::finalize_if_complete`] acts on.
async fn a_running_run(db: &TestDb, planned: i32) -> ScanRunId {
    let run = scans::create_run(&db.pool, None, ScanMode::Full)
        .await
        .expect("create run");
    scans::start_run(&db.pool, run).await.expect("start run");
    scans::add_total_tasks(&db.pool, run, planned)
        .await
        .expect("declare tasks");
    run
}

async fn a_task(db: &TestDb, run: ScanRunId, target: &Json) -> ScanTaskId {
    scans::create_task(&db.pool, run, "series", target)
        .await
        .expect("create task")
        .expect("a genuinely new task")
}

/// `(done_tasks, failed_tasks, total_tasks)`.
async fn counters(db: &TestDb, run: ScanRunId) -> (i32, i32, i32) {
    let run = scans::get_run(&db.pool, run).await.expect("read run");
    (run.done_tasks, run.failed_tasks, run.total_tasks)
}

/// Pin a row's timestamp.
///
/// Both `list_recent_runs` and the failure feed order on a `now()`-stamped column with no
/// tie-break, so rows written microseconds apart have no guaranteed order. Making the instants
/// explicit is what turns "these came back in some order" into an assertion about the `ORDER BY`.
async fn backdate(db: &TestDb, sql: &'static str, id: uuid::Uuid, days_ago: i64) {
    sqlx::query(sql)
        .bind(id)
        .bind(time::OffsetDateTime::now_utc() - time::Duration::days(days_ago))
        .execute(&db.pool)
        .await
        .expect("backdate row");
}

const BACKDATE_RUN: &str = "UPDATE scan_runs SET created_at = $2 WHERE id = $1";
const BACKDATE_TASK: &str = "UPDATE scan_tasks SET finished_at = $2 WHERE id = $1";

async fn task_state(db: &TestDb, task: ScanTaskId) -> TaskState {
    sqlx::query_scalar("SELECT state FROM scan_tasks WHERE id = $1")
        .bind(task.as_uuid())
        .fetch_one(&db.pool)
        .await
        .expect("read task state")
}

async fn attempts(db: &TestDb, task: ScanTaskId) -> i16 {
    sqlx::query_scalar("SELECT attempts FROM scan_tasks WHERE id = $1")
        .bind(task.as_uuid())
        .fetch_one(&db.pool)
        .await
        .expect("read attempts")
}

// ---------------------------------------------------------------------------
// SCAN-1 — settle once
// ---------------------------------------------------------------------------

/// **SCAN-1.** A task contributes exactly one count to its run, no matter how many times it is
/// delivered.
///
/// This is the worker's real code path, in order: `claim_task`, `complete_task`, then — the ack
/// having been lost — `claim_task` and `complete_task` again. Before the fix the second claim
/// moved the row out of `done`, which re-opened `complete_task`'s guard and made `done_tasks` 2
/// for one task.
///
/// The consequence is not a cosmetic counter: [`scans::finalize_if_complete`] fires on
/// `done_tasks + failed_tasks >= total_tasks` and is the run's *only* terminal event, so an
/// inflated count closes a run whose other tasks are still running — the console shows the scan
/// finished, the aggregator stops relaying progress, and whatever those tasks write afterwards is
/// attributed to a run that already ended.
#[tokio::test]
async fn a_redelivered_task_is_counted_once() {
    let db = TestDb::spawn().await;
    let run = a_running_run(&db, 2).await;
    let first = a_task(&db, run, &json!({ "path": "/a" })).await;
    let second = a_task(&db, run, &json!({ "path": "/b" })).await;

    scans::claim_task(&db.pool, first, "worker-1")
        .await
        .expect("claim");
    scans::complete_task(&db.pool, first)
        .await
        .expect("complete");
    assert_eq!(counters(&db, run).await, (1, 0, 2));

    // The ack was lost; JetStream redelivers the very same task.
    scans::claim_task(&db.pool, first, "worker-2")
        .await
        .expect("re-claim");
    assert_eq!(
        task_state(&db, first).await,
        TaskState::Done,
        "a settled task must not be re-opened by a claim"
    );
    scans::complete_task(&db.pool, first)
        .await
        .expect("re-complete");
    assert_eq!(
        counters(&db, run).await,
        (1, 0, 2),
        "the redelivery must not add a second count"
    );

    // And the run must still be waiting for the task that has genuinely not finished.
    assert!(
        scans::finalize_if_complete(&db.pool, run)
            .await
            .expect("finalize")
            .is_none(),
        "the run cannot be complete while one of its two tasks is unsettled"
    );

    scans::complete_task(&db.pool, second)
        .await
        .expect("complete");
    assert_eq!(counters(&db, run).await, (2, 0, 2));
}

/// **SCAN-1.** The first terminal state a task reaches is the one it keeps, and the only one it is
/// counted under.
///
/// Three transitions, each of which used to be allowed by a guard that named only its own target
/// state: `done → failed` (a redelivery of a completed task that errors this time), `failed → done`
/// and `failed → skipped`. Each added a second count, and `failed → done` also erased the error
/// message the operator triage feed reads.
#[tokio::test]
async fn a_settled_task_cannot_move_to_another_terminal_state() {
    let db = TestDb::spawn().await;
    let run = a_running_run(&db, 3).await;
    let completed = a_task(&db, run, &json!({ "path": "/done" })).await;
    let failed = a_task(&db, run, &json!({ "path": "/failed" })).await;
    let skipped = a_task(&db, run, &json!({ "path": "/skipped" })).await;

    scans::complete_task(&db.pool, completed)
        .await
        .expect("complete");
    scans::fail_task(&db.pool, failed, "selector missing")
        .await
        .expect("fail");
    scans::skip_task(&db.pool, skipped).await.expect("skip");
    assert_eq!(counters(&db, run).await, (2, 1, 3), "one settle each");

    scans::fail_task(&db.pool, completed, "late failure")
        .await
        .expect("fail a completed task");
    scans::complete_task(&db.pool, failed)
        .await
        .expect("complete a failed task");
    scans::skip_task(&db.pool, failed)
        .await
        .expect("skip a failed task");
    scans::complete_task(&db.pool, skipped)
        .await
        .expect("complete a skipped task");

    assert_eq!(
        counters(&db, run).await,
        (2, 1, 3),
        "no terminal state may be re-entered under another name"
    );
    assert_eq!(task_state(&db, completed).await, TaskState::Done);
    assert_eq!(task_state(&db, failed).await, TaskState::Failed);
    assert_eq!(task_state(&db, skipped).await, TaskState::Skipped);

    let error: Option<String> = sqlx::query_scalar("SELECT error FROM scan_tasks WHERE id = $1")
        .bind(failed.as_uuid())
        .fetch_one(&db.pool)
        .await
        .expect("read error");
    assert_eq!(
        error.as_deref(),
        Some("selector missing"),
        "the recorded failure reason is what the triage feed shows; a later settle must not clear it"
    );
}

// ---------------------------------------------------------------------------
// Runs
// ---------------------------------------------------------------------------

/// A new run is queued, has nothing planned, and carries no timestamps.
///
/// The console renders "queued" from exactly these columns, and `finalize_if_complete`'s
/// `total_tasks > 0` guard depends on a fresh run starting at zero rather than at some default the
/// schema might supply.
#[tokio::test]
async fn a_new_run_is_queued_with_nothing_planned() {
    let db = TestDb::spawn().await;
    let provider = seed::provider(&db, "alpha").create().await;
    let id = scans::create_run(&db.pool, Some(provider), ScanMode::Fast)
        .await
        .expect("create run");

    let run = scans::get_run(&db.pool, id).await.expect("read run");
    assert_eq!(run.state, RunState::Queued);
    assert_eq!(run.provider_id, Some(provider));
    assert_eq!(run.mode, ScanMode::Fast);
    assert_eq!(
        (run.total_tasks, run.done_tasks, run.failed_tasks),
        (0, 0, 0)
    );
    assert!(run.started_at.is_none());
    assert!(run.finished_at.is_none());
}

/// `get_run` reports a missing run as [`DbError::NotFound`] rather than as an empty success.
///
/// The handler maps this variant to `404`; a `None` reaching it as `Ok` would surface as a `200`
/// with an empty body or a `500`, depending on the caller.
#[tokio::test]
async fn an_unknown_run_is_not_found() {
    let db = TestDb::spawn().await;
    let missing = scans::get_run(&db.pool, ScanRunId::new()).await;
    assert!(matches!(missing, Err(DbError::NotFound)), "{missing:?}");
}

/// `start_run` stamps `started_at` once and never moves it.
///
/// `COALESCE(started_at, now())` is the whole point: the planner's start message is redeliverable,
/// and a plain `started_at = now()` would restamp on every redelivery — so a run's elapsed time
/// would reset itself and a stuck scan would look like it had just begun.
#[tokio::test]
async fn starting_a_run_twice_keeps_the_first_instant() {
    let db = TestDb::spawn().await;
    let id = scans::create_run(&db.pool, None, ScanMode::Full)
        .await
        .expect("create run");

    scans::start_run(&db.pool, id).await.expect("start");
    let first = scans::get_run(&db.pool, id)
        .await
        .expect("read run")
        .started_at
        .expect("started_at is stamped");

    scans::start_run(&db.pool, id).await.expect("re-start");
    let run = scans::get_run(&db.pool, id).await.expect("read run");
    assert_eq!(run.started_at, Some(first));
    assert_eq!(run.state, RunState::Running);
}

/// `list_recent_runs` is newest-first and honours its limit.
///
/// The console's overview is this list, unfiltered: an order that drifted to oldest-first would
/// show an operator a page of finished runs and hide the one they just triggered.
#[tokio::test]
async fn recent_runs_are_newest_first() {
    let db = TestDb::spawn().await;
    let oldest = scans::create_run(&db.pool, None, ScanMode::Full)
        .await
        .expect("create run");
    let middle = scans::create_run(&db.pool, None, ScanMode::Fast)
        .await
        .expect("create run");
    let newest = scans::create_run(&db.pool, None, ScanMode::Full)
        .await
        .expect("create run");
    // Deliberately backdated *against* the insertion order, so passing cannot be an accident of
    // the ids or of the physical row order.
    backdate(&db, BACKDATE_RUN, oldest.as_uuid(), 3).await;
    backdate(&db, BACKDATE_RUN, middle.as_uuid(), 2).await;
    backdate(&db, BACKDATE_RUN, newest.as_uuid(), 1).await;

    let ids: Vec<_> = scans::list_recent_runs(&db.pool, 10)
        .await
        .expect("list runs")
        .iter()
        .map(|r| r.run.id)
        .collect();
    assert_eq!(ids, vec![newest, middle, oldest]);

    let ids: Vec<_> = scans::list_recent_runs(&db.pool, 2)
        .await
        .expect("list runs")
        .iter()
        .map(|r| r.run.id)
        .collect();
    assert_eq!(ids, vec![newest, middle]);
}

/// `finalize_if_complete` transitions a run exactly once, and only when every planned task has
/// settled.
///
/// It is the run's single terminal event: the control-plane aggregator calls it on **every**
/// progress message, so returning `Some` twice publishes two terminal events for one run, and
/// returning `Some` early publishes one for a run that is still working. `state = 'running'` in the
/// `WHERE` is what makes the second call a no-op — that is the atomic claim, not a comment.
#[tokio::test]
async fn a_run_finalizes_once_and_only_when_every_task_has_settled() {
    let db = TestDb::spawn().await;
    let run = a_running_run(&db, 2).await;
    let first = a_task(&db, run, &json!({ "path": "/a" })).await;
    let second = a_task(&db, run, &json!({ "path": "/b" })).await;

    assert!(
        scans::finalize_if_complete(&db.pool, run)
            .await
            .expect("finalize")
            .is_none(),
        "nothing has settled yet"
    );

    scans::complete_task(&db.pool, first)
        .await
        .expect("complete");
    assert!(
        scans::finalize_if_complete(&db.pool, run)
            .await
            .expect("finalize")
            .is_none(),
        "one of two tasks is not enough"
    );

    scans::complete_task(&db.pool, second)
        .await
        .expect("complete");
    let finalized = scans::finalize_if_complete(&db.pool, run)
        .await
        .expect("finalize")
        .expect("the run is complete");
    assert_eq!(finalized.state, RunState::Completed);
    assert!(finalized.finished_at.is_some());

    assert!(
        scans::finalize_if_complete(&db.pool, run)
            .await
            .expect("finalize")
            .is_none(),
        "a second caller must not emit a second terminal event"
    );
}

/// A run is `failed` only when **every** task failed; one success makes it `completed`.
///
/// The `CASE` reads `done_tasks = 0 AND failed_tasks > 0`, which is a judgement rather than a
/// mechanism: a partially-failed scan did produce catalogue data and must not be presented — or
/// alerted on — as a total failure. Inverting the test is invisible until a scan has a mix.
#[tokio::test]
async fn only_a_wholly_failed_run_is_failed() {
    let db = TestDb::spawn().await;

    let all_failed = a_running_run(&db, 2).await;
    for path in ["/a", "/b"] {
        let task = a_task(&db, all_failed, &json!({ "path": path })).await;
        scans::fail_task(&db.pool, task, "boom")
            .await
            .expect("fail");
    }
    let run = scans::finalize_if_complete(&db.pool, all_failed)
        .await
        .expect("finalize")
        .expect("complete");
    assert_eq!(run.state, RunState::Failed);

    let partial = a_running_run(&db, 2).await;
    let ok = a_task(&db, partial, &json!({ "path": "/a" })).await;
    let bad = a_task(&db, partial, &json!({ "path": "/b" })).await;
    scans::complete_task(&db.pool, ok).await.expect("complete");
    scans::fail_task(&db.pool, bad, "boom").await.expect("fail");
    let run = scans::finalize_if_complete(&db.pool, partial)
        .await
        .expect("finalize")
        .expect("complete");
    assert_eq!(
        run.state,
        RunState::Completed,
        "a partially-failed run still completed"
    );

    // A skipped task counts as done, so a run of nothing but skips is a completion.
    let skipped = a_running_run(&db, 1).await;
    let task = a_task(&db, skipped, &json!({ "path": "/a" })).await;
    scans::skip_task(&db.pool, task).await.expect("skip");
    let run = scans::finalize_if_complete(&db.pool, skipped)
        .await
        .expect("finalize")
        .expect("complete");
    assert_eq!(run.state, RunState::Completed);
}

/// A `running` run with nothing planned is never finalised, and neither is a run that is not
/// `running`.
///
/// `total_tasks > 0` is the guard against the degenerate `0 >= 0`: a run whose planner has not
/// fanned out yet satisfies `done + failed >= total` trivially, and without the guard the very
/// first progress message would finalise every run before its first task existed.
#[tokio::test]
async fn finalize_ignores_an_unplanned_run_and_a_run_that_is_not_running() {
    let db = TestDb::spawn().await;

    let unplanned = scans::create_run(&db.pool, None, ScanMode::Full)
        .await
        .expect("create run");
    scans::start_run(&db.pool, unplanned).await.expect("start");
    assert!(
        scans::finalize_if_complete(&db.pool, unplanned)
            .await
            .expect("finalize")
            .is_none(),
        "0 >= 0 must not finalise a run whose fan-out has not happened"
    );

    let cancelled = a_running_run(&db, 1).await;
    let task = a_task(&db, cancelled, &json!({ "path": "/a" })).await;
    scans::finish_run(&db.pool, cancelled, RunState::Cancelled)
        .await
        .expect("cancel");
    scans::complete_task(&db.pool, task)
        .await
        .expect("complete");
    assert!(
        scans::finalize_if_complete(&db.pool, cancelled)
            .await
            .expect("finalize")
            .is_none(),
        "a cancelled run must not be resurrected as completed by a late task"
    );
    assert_eq!(
        scans::get_run(&db.pool, cancelled)
            .await
            .expect("read run")
            .state,
        RunState::Cancelled
    );
}

// ---------------------------------------------------------------------------
// Tasks
// ---------------------------------------------------------------------------

/// Creating the same `(run, kind, target)` twice inserts once, and the caller can tell which call
/// was the real one.
///
/// `create_task` returning `None` is what stops a redelivered `catalog_page` from re-enqueuing —
/// and, crucially, from calling `add_total_tasks` again. If it reported an id both times the run's
/// planned total would grow on every redelivery and the run would never satisfy
/// `done + failed >= total`, so it would hang in `running` forever.
///
/// The `jsonb` key order is deliberately different on the second call: the unique index is on
/// `(target::text)`, and `jsonb` normalises key order on the way in, so the two spellings are one
/// target. On a `json`/`text` column they would be two, and every reordered target would be
/// double-enqueued.
#[tokio::test]
async fn creating_a_task_is_idempotent_on_the_run_kind_and_target() {
    let db = TestDb::spawn().await;
    let run = a_running_run(&db, 0).await;

    let first = scans::create_task(&db.pool, run, "series", &json!({ "a": 1, "b": 2 }))
        .await
        .expect("create task");
    assert!(first.is_some());

    let repeat = scans::create_task(&db.pool, run, "series", &json!({ "b": 2, "a": 1 }))
        .await
        .expect("create task");
    assert!(
        repeat.is_none(),
        "the same target spelled with its keys reordered is the same task"
    );

    // A different kind, or a different run, is a different task.
    assert!(
        scans::create_task(&db.pool, run, "catalog_page", &json!({ "a": 1, "b": 2 }))
            .await
            .expect("create task")
            .is_some()
    );
    let other_run = a_running_run(&db, 0).await;
    assert!(
        scans::create_task(&db.pool, other_run, "series", &json!({ "a": 1, "b": 2 }))
            .await
            .expect("create task")
            .is_some()
    );
}

/// `create_tasks` reports only the rows it actually inserted, including when a batch repeats a
/// target within itself.
///
/// The caller derives `add_total_tasks(delta)` and its publish list from the returned rows, so an
/// over-report inflates the run's planned total (which never completes) and an under-report leaves
/// a task in the table that nothing dispatches. Duplicates *within* one statement are the case
/// that needs `DO NOTHING` rather than `DO UPDATE`: the latter cannot touch one row twice and
/// raises `ON CONFLICT DO UPDATE command cannot affect row a second time`.
#[tokio::test]
async fn creating_a_batch_of_tasks_reports_only_the_new_ones() {
    let db = TestDb::spawn().await;
    let run = a_running_run(&db, 0).await;

    assert!(
        scans::create_tasks(&db.pool, run, "series", &[])
            .await
            .expect("empty batch")
            .is_empty(),
        "an empty batch must not reach the database at all"
    );

    let existing = json!({ "path": "/known" });
    a_task(&db, run, &existing).await;

    let batch = vec![
        existing.clone(),
        json!({ "path": "/new-1" }),
        json!({ "path": "/new-2" }),
        // The same page listing one path twice.
        json!({ "path": "/new-2" }),
    ];
    let inserted = scans::create_tasks(&db.pool, run, "series", &batch)
        .await
        .expect("create tasks");
    let mut paths: Vec<String> = inserted
        .iter()
        .map(|(_, target)| target["path"].as_str().expect("a path").to_owned())
        .collect();
    paths.sort();
    assert_eq!(paths, vec!["/new-1", "/new-2"]);

    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM scan_tasks WHERE run_id = $1")
        .bind(run.as_uuid())
        .fetch_one(&db.pool)
        .await
        .expect("count tasks");
    assert_eq!(total, 3, "three distinct targets exist, not five");
}

/// A claim records the worker and counts an attempt, and only ever moves a task forward.
///
/// `attempts` is what the worker's delivery ceiling and the operator triage feed are read against,
/// so it has to count deliveries rather than tasks. The state guard is SCAN-1: a settled task is
/// not re-claimed, so a redelivery leaves both the state and the attempt count where they were.
#[tokio::test]
async fn claiming_records_the_worker_and_counts_the_attempt() {
    let db = TestDb::spawn().await;
    let run = a_running_run(&db, 1).await;
    let task = a_task(&db, run, &json!({ "path": "/a" })).await;

    assert_eq!(attempts(&db, task).await, 0);
    scans::claim_task(&db.pool, task, "worker-1")
        .await
        .expect("claim");
    assert_eq!(attempts(&db, task).await, 1);
    assert_eq!(task_state(&db, task).await, TaskState::Claimed);

    // A second delivery before the task settles is a genuine second attempt.
    scans::claim_task(&db.pool, task, "worker-2")
        .await
        .expect("re-claim");
    assert_eq!(attempts(&db, task).await, 2);

    let worker: Option<String> =
        sqlx::query_scalar("SELECT worker_id FROM scan_tasks WHERE id = $1")
            .bind(task.as_uuid())
            .fetch_one(&db.pool)
            .await
            .expect("read worker_id");
    assert_eq!(worker.as_deref(), Some("worker-2"));

    // After settling, neither moves again.
    scans::complete_task(&db.pool, task)
        .await
        .expect("complete");
    scans::claim_task(&db.pool, task, "worker-3")
        .await
        .expect("claim a done task");
    assert_eq!(attempts(&db, task).await, 2);
    assert_eq!(task_state(&db, task).await, TaskState::Done);
}

/// The durable fallback claim takes one queued task at a time, scoped to its run, and reports
/// `None` when there is nothing left.
///
/// This is the path used when the broker is unavailable, so it is the one that must not hand the
/// same task to two workers: `FOR UPDATE SKIP LOCKED` plus `state = 'queued'` is the whole
/// mechanism. `ORDER BY id` is FIFO only because the ids are `UUIDv7` — a switch to v4 would silently
/// randomise the order, which is why the ordering is asserted rather than assumed.
#[tokio::test]
async fn the_fallback_claim_takes_one_queued_task_per_call_within_its_run() {
    let db = TestDb::spawn().await;
    let run = a_running_run(&db, 2).await;
    let other_run = a_running_run(&db, 1).await;
    let first = a_task(&db, run, &json!({ "path": "/a" })).await;
    let second = a_task(&db, run, &json!({ "path": "/b" })).await;
    let elsewhere = a_task(&db, other_run, &json!({ "path": "/c" })).await;

    let claimed = scans::claim_next_queued(&db.pool, run, "worker-1")
        .await
        .expect("claim")
        .expect("a queued task");
    assert_eq!(claimed.id, first, "oldest first");
    assert_eq!(claimed.state, TaskState::Claimed);
    assert_eq!(claimed.worker_id.as_deref(), Some("worker-1"));
    assert_eq!(claimed.attempts, 1);

    let claimed = scans::claim_next_queued(&db.pool, run, "worker-2")
        .await
        .expect("claim")
        .expect("a queued task");
    assert_eq!(claimed.id, second);

    assert!(
        scans::claim_next_queued(&db.pool, run, "worker-3")
            .await
            .expect("claim")
            .is_none(),
        "a claimed task is not queued, so there is nothing left in this run"
    );
    assert_eq!(
        task_state(&db, elsewhere).await,
        TaskState::Queued,
        "another run's task must not be claimable through this run"
    );
}

/// The triage feed reports failures only, newest first, with the run context an operator needs —
/// and survives the provider being deleted.
///
/// Three things drift silently here. The `WHERE t.state = 'failed'` is the difference between a
/// triage feed and a task dump. The `LEFT JOIN providers` is why deleting a provider does not erase
/// the record of what its scans did — an inner join would drop those rows entirely, which is the
/// worst possible outcome for an audit surface. And `NULLS LAST` keeps a failure with no
/// `finished_at` from sorting to the top of the page ahead of every real one.
#[tokio::test]
async fn the_failed_task_feed_reports_failures_with_their_run_context() {
    let db = TestDb::spawn().await;
    let provider = seed::provider(&db, "alpha").create().await;
    let run = scans::create_run(&db.pool, Some(provider), ScanMode::Full)
        .await
        .expect("create run");
    scans::start_run(&db.pool, run).await.expect("start");
    scans::add_total_tasks(&db.pool, run, 3)
        .await
        .expect("declare");

    let ok = a_task(&db, run, &json!({ "path": "/ok" })).await;
    let older = a_task(&db, run, &json!({ "path": "/older" })).await;
    let newer = a_task(&db, run, &json!({ "path": "/newer" })).await;
    scans::complete_task(&db.pool, ok).await.expect("complete");
    scans::fail_task(&db.pool, older, "selector missing")
        .await
        .expect("fail");
    scans::fail_task(&db.pool, newer, "http 503")
        .await
        .expect("fail");
    backdate(&db, BACKDATE_TASK, older.as_uuid(), 2).await;
    backdate(&db, BACKDATE_TASK, newer.as_uuid(), 1).await;

    let feed = scans::failed_tasks_filtered(&db.pool, None, None, false, 10)
        .await
        .expect("failed tasks");
    assert_eq!(feed.len(), 2, "the completed task is not a failure");
    assert_eq!(feed[0].id, newer.as_uuid(), "newest first");
    assert_eq!(feed[0].error.as_deref(), Some("http 503"));
    assert_eq!(feed[0].provider_slug.as_deref(), Some("alpha"));
    assert_eq!(feed[0].mode, "full");
    assert_eq!(feed[0].kind, "series");
    assert_eq!(feed[1].id, older.as_uuid());

    // Deleting the provider must leave the failure record intact, with no slug.
    providers::delete(&db.pool, provider)
        .await
        .expect("delete provider");
    let feed = scans::failed_tasks_filtered(&db.pool, None, None, false, 10)
        .await
        .expect("failed tasks");
    assert_eq!(feed.len(), 2, "an inner join would have dropped both rows");
    assert!(feed.iter().all(|t| t.provider_slug.is_none()));
}

// ---------------------------------------------------------------------------
// Coalescing — one in-flight run per provider and mode
// ---------------------------------------------------------------------------

/// A run in flight, holding one unsettled task — what the planner must refuse to duplicate.
async fn a_run_in_flight(db: &TestDb, provider: ProviderId, mode: ScanMode) -> ScanRunId {
    let run = scans::create_run(&db.pool, Some(provider), mode)
        .await
        .expect("create run");
    scans::start_run(&db.pool, run).await.expect("start run");
    scans::add_total_tasks(&db.pool, run, 1)
        .await
        .expect("declare tasks");
    a_task(db, run, &json!({})).await;
    run
}

const AN_HOUR: std::time::Duration = std::time::Duration::from_secs(3600);

/// **The clogged queue.** A provider whose fast scan is still running must not be given a second
/// one.
///
/// The scheduler sweeps on a fixed interval; a fast scan takes as long as the provider's feed and
/// crawl budget make it take. When the second outgrows the first, every tick used to plan another
/// identical `latest_feed` run — so that provider's lane filled with re-reads of a feed it was
/// already reading, and because the worker drains the whole fast tier before it looks at the full
/// one, no full scan anywhere in the deployment was served either.
#[tokio::test]
async fn a_provider_already_scanning_does_not_get_a_second_run() {
    let db = TestDb::spawn().await;
    let provider = seed::provider(&db, "alpha").create().await;
    let running = a_run_in_flight(&db, provider, ScanMode::Fast).await;

    assert_eq!(
        scans::in_flight_run(&db.pool, provider, ScanMode::Fast, AN_HOUR)
            .await
            .expect("query"),
        Some(running)
    );
}

/// Coalescing is per provider **and** per mode: neither a second provider nor the other mode is
/// suppressed by a run that has nothing to do with it. Getting this wrong would be silent — one
/// slow provider would simply stop the whole deployment scanning.
#[tokio::test]
async fn coalescing_is_scoped_to_the_provider_and_the_mode() {
    let db = TestDb::spawn().await;
    let alpha = seed::provider(&db, "alpha").create().await;
    let beta = seed::provider(&db, "beta").create().await;
    a_run_in_flight(&db, alpha, ScanMode::Fast).await;

    assert_eq!(
        scans::in_flight_run(&db.pool, alpha, ScanMode::Full, AN_HOUR)
            .await
            .expect("query"),
        None,
        "a fast scan must not suppress a full one"
    );
    assert_eq!(
        scans::in_flight_run(&db.pool, beta, ScanMode::Fast, AN_HOUR)
            .await
            .expect("query"),
        None,
        "one provider's scan must not suppress another's"
    );
}

/// A run whose tasks have all settled is **not** in flight, even while its row still says
/// `running`.
///
/// Finalisation happens when the aggregator sees the last progress event, and that event is
/// best-effort — a lost one leaves the row `running` with nothing left to do. Treating the state
/// alone as "in flight" would let one dropped event stop the provider being scanned again until
/// the staleness bound expired.
#[tokio::test]
async fn a_run_with_nothing_left_to_do_does_not_suppress_the_next_one() {
    let db = TestDb::spawn().await;
    let provider = seed::provider(&db, "alpha").create().await;
    let run = a_run_in_flight(&db, provider, ScanMode::Fast).await;
    let task = scans::claim_next_queued(&db.pool, run, "worker-1")
        .await
        .expect("claim")
        .expect("the run's only task");
    scans::complete_task(&db.pool, task.id)
        .await
        .expect("complete");

    assert_eq!(
        scans::in_flight_run(&db.pool, provider, ScanMode::Fast, AN_HOUR)
            .await
            .expect("query"),
        None
    );
}

/// A run whose task will never be delivered stops suppressing once it is old enough.
///
/// The planner persists a task row and then publishes it; a crash between the two leaves a run
/// that is indistinguishable from a slow one and will never settle on its own. Age is the only
/// thing that separates them, and without this bound that single lost publish would retire the
/// provider from scanning for the life of the deployment.
#[tokio::test]
async fn a_run_that_can_never_settle_stops_suppressing_when_it_goes_stale() {
    let db = TestDb::spawn().await;
    let provider = seed::provider(&db, "alpha").create().await;
    let run = a_run_in_flight(&db, provider, ScanMode::Fast).await;
    backdate(&db, BACKDATE_RUN, run.as_uuid(), 1).await;

    assert_eq!(
        scans::in_flight_run(&db.pool, provider, ScanMode::Fast, AN_HOUR)
            .await
            .expect("query"),
        None,
        "a day-old run is past an hour's staleness bound"
    );
}

/// A run the planner has created but not yet given a task to is already in flight.
///
/// It is the narrowest window in the planner — one statement wide — and the only one where the
/// unsettled-task test says "nothing to do" about a run that has not started doing it. A
/// concurrent sweep and "Scan now" landing inside it would otherwise produce exactly the
/// duplicate run this whole guard exists to prevent.
#[tokio::test]
async fn a_run_whose_first_task_is_not_written_yet_is_already_in_flight() {
    let db = TestDb::spawn().await;
    let provider = seed::provider(&db, "alpha").create().await;
    let run = scans::create_run(&db.pool, Some(provider), ScanMode::Fast)
        .await
        .expect("create run");

    assert_eq!(
        scans::in_flight_run(&db.pool, provider, ScanMode::Fast, AN_HOUR)
            .await
            .expect("query"),
        Some(run)
    );
}

/// A finished run never suppresses the next one, whichever terminal state it reached.
#[tokio::test]
async fn a_finished_run_does_not_suppress_the_next_one() {
    let db = TestDb::spawn().await;
    let provider = seed::provider(&db, "alpha").create().await;
    for state in [RunState::Completed, RunState::Failed, RunState::Cancelled] {
        let run = a_run_in_flight(&db, provider, ScanMode::Fast).await;
        scans::finish_run(&db.pool, run, state)
            .await
            .expect("finish run");
        assert_eq!(
            scans::in_flight_run(&db.pool, provider, ScanMode::Fast, AN_HOUR)
                .await
                .expect("query"),
            None,
            "a {state:?} run still reads as in flight"
        );
    }
}
