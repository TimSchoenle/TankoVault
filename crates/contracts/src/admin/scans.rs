//! Scan runs and tasks: listings, window health, live activity, run detail and the failure feed.

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use tankovault_domain::{ProviderId, RunState, ScanMode, ScanRunId, TaskState};
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

/// A scan run as the console reads it: the persisted row plus the slug of the provider it was
/// scoped to.
///
/// Published under the `ScanRun` component name. The slug lets the console render the provider
/// by name and match a run the live stream pushes against the filter an operator typed; a bare
/// `provider_id` allows neither.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = ScanRun)]
pub struct ScanRunView {
    /// The run.
    pub id: ScanRunId,
    /// `null` for a run covering every enabled provider, and once the named provider is
    /// deleted.
    pub provider_id: Option<ProviderId>,
    /// `null` for an all-provider run, and for a run whose provider was deleted.
    pub provider_slug: Option<String>,
    /// Whether the run rebuilds the archive or reads the latest feed.
    pub mode: ScanMode,
    /// Where the run is in its lifecycle.
    pub state: RunState,
    /// Tasks the run dispatched.
    pub total_tasks: i32,
    /// Tasks that finished successfully.
    pub done_tasks: i32,
    /// Tasks that gave up.
    pub failed_tasks: i32,
    /// When the first task was claimed, `null` while the run is still queued.
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub started_at: Option<OffsetDateTime>,
    /// When the run settled, `null` while it is still going.
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub finished_at: Option<OffsetDateTime>,
    /// When the run was enqueued.
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub created_at: OffsetDateTime,
}

/// A page of scan runs plus how many the filter matches in total.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ScanRunPageView {
    /// The page, newest run first.
    pub items: Vec<ScanRunView>,
    /// Total matching the current filter, ignoring `limit` and `offset`.
    pub total: i64,
}

/// What the scan filter matched, as figures.
///
/// Scoped by the same provider and window as the row list, so a narrowed filter reports its own
/// success rate. The reader divides: `tasks_done` and `tasks_total` let the panel show both the
/// ratio and the magnitude behind it, which a lone percentage cannot.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ScanSummaryView {
    /// Runs the filter matched.
    pub runs_total: i64,
    /// Of those, still waiting to start.
    pub runs_queued: i64,
    /// Of those, in flight.
    pub runs_running: i64,
    /// Of those, finished with at least one task done.
    pub runs_completed: i64,
    /// Of those, finished with every task failed, which is what marks a run failed.
    pub runs_failed: i64,
    /// Of those, stopped by an operator.
    pub runs_cancelled: i64,
    /// Tasks those runs dispatched.
    pub tasks_total: i64,
    /// Of those, settled successfully. The numerator the panel divides itself.
    pub tasks_done: i64,
    /// Of those, settled as failures.
    pub tasks_failed: i64,
    /// Failures still in the triage feed. `tasks_failed` counts every failure in the window,
    /// including the ones an operator has cleared.
    pub failures_open: i64,
    /// Summed run wall-clock in seconds; a run in flight counts up to now.
    pub busy_seconds: f64,
    /// Creation time of the oldest matching run, `null` when the filter matched none.
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub first_run_at: Option<OffsetDateTime>,
    /// Creation time of the newest matching run, `null` when the filter matched none.
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub last_run_at: Option<OffsetDateTime>,
    /// Per-provider health over the same window, worst first. Providers with neither a run nor
    /// an open failure are omitted.
    pub providers: Vec<ProviderScanHealthView>,
}

/// One provider's scan health over the summary's window.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ProviderScanHealthView {
    /// The provider, as the console filter spells it.
    pub slug: String,
    /// Its display name.
    pub name: String,
    /// Runs it had in the window.
    pub runs: i64,
    /// Of those, still queued or running.
    pub runs_active: i64,
    /// Of those, failed.
    pub runs_failed: i64,
    /// Tasks of those runs that settled successfully.
    pub tasks_done: i64,
    /// Tasks of those runs that failed.
    pub tasks_failed: i64,
    /// Failures still in the triage feed for this provider.
    pub failures_open: i64,
    /// Its most recent run in the window, `null` when it had none.
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub last_run_at: Option<OffsetDateTime>,
    /// Its most recent failure in the window, `null` when it had none.
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub last_failure_at: Option<OffsetDateTime>,
}

/// The task-level state of the runs in flight, pushed on the console stream.
///
/// Run counters alone cannot tell "working" from "wedged", since both leave `done_tasks` where it
/// was. These figures can: what a worker is holding, since when, and what settled most recently.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ScanActivityView {
    /// One entry per run in flight, and none for a run that has settled.
    pub runs: Vec<RunActivityView>,
    /// The most recently settled tasks of the runs in flight, newest first. Empty when nothing
    /// is running, instead of a replay of the last scan.
    pub events: Vec<TaskEventView>,
}

/// One in-flight run's task breakdown.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RunActivityView {
    /// The run this breakdown belongs to.
    pub run_id: ScanRunId,
    /// Tasks nobody has claimed yet.
    pub queued_tasks: i64,
    /// Tasks a worker is holding right now.
    pub running_tasks: i64,
    /// When the oldest still-held task was claimed. A claim instant that stops moving is the
    /// first visible symptom of a wedged worker.
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub oldest_claim_at: Option<OffsetDateTime>,
    /// Task kinds in flight, sorted.
    pub kinds: Vec<String>,
    /// Distinct workers holding a task right now.
    pub workers: i64,
    /// What the run's oldest held task is doing, as a [`tankovault_domain::ScanStage`] token.
    /// Omitted when nothing is held; with `waiting_since`, that tells the console "working" from
    /// "waiting for a worker".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
    /// When that task entered that stage. A stage stamp that stops moving is the symptom the run
    /// counters cannot show.
    #[serde(default, with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub stage_at: Option<OffsetDateTime>,
    /// Progress inside the stage, where the stage counts anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage_done: Option<i32>,
    /// What the stage counts up to, where it counts at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage_total: Option<i32>,
    /// What the stage is working against, such as a series path or a catalogue page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage_detail: Option<String>,
    /// When the run's oldest still-queued task was created.
    ///
    /// With `running_tasks` at zero, the run is waiting for a worker slot. A worker runs one task
    /// per provider, so a provider's second run queues behind its first, and through `state`
    /// alone that looks the same as a run that has hung.
    #[serde(default, with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub waiting_since: Option<OffsetDateTime>,
}

/// What a cancellation stopped.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ScanCancelledView {
    /// Runs moved to `cancelled`. Runs already terminal are not counted, so two operators
    /// cancelling the same queue do not both claim it.
    pub runs: i64,
    /// Tasks abandoned across those runs. They do not count as done: a cancelled run must not
    /// render as a completed one.
    pub tasks: i64,
}

/// Which in-flight runs a bulk cancellation stops.
///
/// Both fields narrow. A body with neither stops everything queued or running.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct CancelScansBody {
    /// Provider slug.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Narrows the cancellation to one cadence, so a fast sweep can be stopped without
    /// touching an archive rebuild that has been running for an hour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<ScanMode>,
}

/// One run's tasks and the breakdown of where its time went.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ScanRunDetailView {
    /// Where the run's time went, summed over its tasks.
    pub telemetry: RunTelemetryView,
    /// Summed milliseconds per stage across the run, largest first: what the run spent its time
    /// on.
    pub stages: Vec<StageTotalView>,
    /// Every task of the run, whatever its state.
    pub tasks: Vec<ScanTaskDetailView>,
}

/// A run's time, summed over every task that recorded a breakdown.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RunTelemetryView {
    /// Settled tasks carrying a breakdown. Every other figure sums over exactly these, so a run
    /// with no recorded breakdown reports zero, not a wrong total.
    pub tasks_measured: i64,
    /// Summed execution time in milliseconds. Exceeds the run's wall clock when tasks ran
    /// concurrently: it is work performed, not time elapsed.
    pub busy_ms: i64,
    /// Summed time tasks spent created but unclaimed, queued behind a busy provider.
    pub wait_ms: i64,
    /// Outbound requests the run made.
    pub requests: i64,
    /// Milliseconds spent inside those requests.
    pub fetch_ms: i64,
    /// Milliseconds spent waiting for permission to send a request: the concurrency gate, the
    /// token rate, the crawl delay and any adaptive 429 penalty. Read against `busy_ms`, this is
    /// what separates "the scan is broken" from "the provider's crawl budget is small".
    pub pace_wait_ms: i64,
    /// Milliseconds spent waiting on a challenge solver.
    pub solver_ms: i64,
    /// Solve attempts, whether or not they succeeded.
    pub solver_calls: i64,
    /// Responses the provider answered with 429 or 503, each of which widened its spacing.
    pub throttled: i64,
}

/// Summed milliseconds for one stage across a run.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct StageTotalView {
    /// A [`tankovault_domain::ScanStage`] token.
    pub stage: String,
    /// Milliseconds in this stage, summed across the run.
    pub millis: i64,
    /// Tasks that reported this stage at all.
    pub tasks: i64,
}

/// One task of a run, with its stage and what it cost.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ScanTaskDetailView {
    /// The task.
    pub id: Uuid,
    /// What the task was asked to do: a catalogue page, a series, a latest feed.
    pub kind: String,
    /// The task's target, shaped by `kind`.
    pub target: Json,
    /// Where the task is in its lifecycle.
    pub state: TaskState,
    /// Claims taken on it, so a retried task reads higher than one.
    pub attempts: i16,
    /// The worker holding it, `null` before the first claim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_id: Option<String>,
    /// Why it failed, `null` unless it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The stage the task is in, or the one it ended in. On a failure it says whether the
    /// provider stopped answering or the ingest rejected what it sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
    /// When it entered that stage.
    #[serde(default, with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub stage_at: Option<OffsetDateTime>,
    /// Progress inside the stage, where the stage counts anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage_done: Option<i32>,
    /// What that progress counts up to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage_total: Option<i32>,
    /// What the stage is working against: a series path, a catalogue page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage_detail: Option<String>,
    /// `null` when the task recorded none.
    #[serde(default, with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub created_at: Option<OffsetDateTime>,
    /// When a worker last took it, `null` while queued.
    #[serde(default, with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub claimed_at: Option<OffsetDateTime>,
    /// When it settled, `null` while it has not.
    #[serde(default, with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub finished_at: Option<OffsetDateTime>,
    /// Milliseconds between creation and claim: time nobody was working on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_ms: Option<i32>,
    /// Milliseconds between claim and settle: time someone was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i32>,
    /// The task's own [`tankovault_domain::StageTimings`], as recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub telemetry: Option<Json>,
}

/// One settled task in the live tail.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TaskEventView {
    /// The task.
    pub id: Uuid,
    /// The run it belonged to.
    pub run_id: ScanRunId,
    /// Provider of the owning run, `null` once that provider is deleted.
    pub provider_slug: Option<String>,
    /// What the task was asked to do.
    pub kind: String,
    /// How it settled, which for a tail entry is `done`, `failed` or `skipped`.
    pub state: TaskState,
    /// What the task was pointed at, as the planner wrote it.
    pub target: Json,
    /// Why it failed, `null` unless it did.
    pub error: Option<String>,
    /// Claims taken on it before it settled.
    pub attempts: i16,
    /// When it settled, which is what the tail is ordered by.
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub finished_at: Option<OffsetDateTime>,
}

/// Which failures to clear out of the triage feed. Every field narrows; a body with none of them
/// clears the whole feed.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct ClearFailuresBody {
    /// Provider slug.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Inclusive lower bound on the failure's `finished_at`, RFC 3339.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
    /// One run's failures.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<ScanRunId>,
    /// One error group, exactly as the grouped feed reported it. Omitted and `null` read the same,
    /// and `match_null_error` then decides what is selected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// With no `error`, clear only the group that recorded no error. Unset, no `error` selects
    /// every error.
    #[serde(default)]
    pub match_null_error: bool,
}

/// How many failures a clear hid.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FailuresClearedView {
    /// Rows this call acknowledged. Already-cleared rows are excluded, so two operators clearing
    /// the same feed do not both claim it.
    pub cleared: i64,
}

/// One distinct scan failure, with how often it happened and which providers it hit.
///
/// The grouped view of the failure feed: twelve rows of the same broken selector are one
/// problem, which the flat feed shows as twelve.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FailureGroupView {
    /// The error text these failures share. `null` groups the failures that recorded none.
    pub error: Option<String>,
    /// Failures sharing this error, cleared ones counted only when the caller asked for them.
    pub count: i64,
    /// How many of `count` an operator has already cleared. Non-zero only when the caller asked
    /// for cleared failures back.
    pub cleared: i64,
    /// Provider slugs affected, sorted.
    pub providers: Vec<String>,
    /// Task kinds this error struck, sorted. The same message on a `series` task and on a
    /// `catalog_page` task is two problems.
    pub kinds: Vec<String>,
    /// The most recent of them, `null` only if the group is empty.
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub latest_at: Option<OffsetDateTime>,
}

/// A failed scan task with its run's provider and mode, for the console error feed.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FailedTaskView {
    /// The failed task.
    pub id: Uuid,
    /// The run it belonged to.
    pub run_id: Uuid,
    /// Provider slug of the owning run, `null` once the provider is deleted.
    pub provider_slug: Option<String>,
    /// Cadence of the owning run: `full` or `fast`.
    pub mode: String,
    /// What the task was asked to do.
    pub kind: String,
    /// Why it failed, `null` for a task that recorded no message.
    pub error: Option<String>,
    /// Claims taken before it gave up.
    pub attempts: i16,
    /// When it gave up.
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub finished_at: Option<OffsetDateTime>,
    /// When an operator cleared this failure from the triage feed, if they have.
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub acknowledged_at: Option<OffsetDateTime>,
}

/// What the control plane answers when a scan is planned: the runs it created.
///
/// Published by `services/control-plane` on its internal `POST /internal/scans`, and
/// republished verbatim by `services/api` on `POST /v1/admin/scans` and
/// `POST /v1/admin/providers/{id}/resolve`. Both ends name this type, so removing `run_ids`
/// fails to build at the producer and at the republisher, and the console's "N scans queued"
/// reads the field the planner writes.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = ScanTriggered)]
pub struct ScanTriggeredView {
    /// One id per provider scanned, or per active provider when the request names none. A
    /// provider already scanning in this mode contributes that run's id, so an id here means
    /// "this run covers your request", not always "this run is new".
    pub run_ids: Vec<ScanRunId>,
}

#[cfg(test)]
mod tests {
    use super::{
        ClearFailuresBody, FailedTaskView, FailureGroupView, FailuresClearedView,
        ProviderScanHealthView, RunActivityView, ScanActivityView, ScanRunPageView, ScanRunView,
        ScanSummaryView, TaskEventView,
    };
    use serde_json::json;
    use tankovault_domain::{ProviderId, RunState, ScanMode, ScanRunId, TaskState};
    use time::OffsetDateTime;
    use uuid::Uuid;

    fn an_instant() -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_754_700_000).expect("a representable instant")
    }

    fn a_run() -> ScanRunView {
        ScanRunView {
            id: ScanRunId::new(),
            provider_id: Some(ProviderId::new()),
            provider_slug: Some("kunmanga".to_owned()),
            mode: ScanMode::Fast,
            state: RunState::Running,
            total_tasks: 120,
            done_tasks: 90,
            failed_tasks: 4,
            started_at: Some(an_instant()),
            finished_at: None,
            created_at: an_instant(),
        }
    }

    /// The history table renders the run page, and its stamps are `OffsetDateTime` behind an
    /// RFC 3339 serde attribute, a mistake the type system cannot catch. The two workspaces
    /// share only `openapi.json`, so a field that does not survive its round trip is a value the
    /// operator never sees.
    #[test]
    fn a_run_page_round_trips() {
        let page = ScanRunPageView {
            items: vec![a_run()],
            total: 412,
        };
        let encoded = serde_json::to_string(&page).expect("serialize the run page");
        let decoded: ScanRunPageView = serde_json::from_str(&encoded).expect("read it back");
        assert_eq!(decoded.total, 412);
        assert_eq!(decoded.items[0].provider_slug.as_deref(), Some("kunmanga"));
        assert_eq!(decoded.items[0].started_at, Some(an_instant()));
        assert_eq!(
            decoded.items[0].finished_at, None,
            "a run in flight has no end"
        );
        assert_eq!(decoded.items[0].done_tasks, 90);
    }

    /// `busy_seconds` is the panel's only float, and every rate it shows divides by it.
    #[test]
    fn a_window_summary_round_trips() {
        let summary = ScanSummaryView {
            runs_total: 12,
            runs_queued: 1,
            runs_running: 2,
            runs_completed: 8,
            runs_failed: 1,
            runs_cancelled: 0,
            tasks_total: 4_000,
            tasks_done: 3_800,
            tasks_failed: 200,
            failures_open: 37,
            busy_seconds: 942.5,
            first_run_at: Some(an_instant()),
            last_run_at: Some(an_instant()),
            providers: vec![ProviderScanHealthView {
                slug: "kunmanga".to_owned(),
                name: "KunManga".to_owned(),
                runs: 4,
                runs_active: 1,
                runs_failed: 1,
                tasks_done: 900,
                tasks_failed: 100,
                failures_open: 37,
                last_run_at: Some(an_instant()),
                last_failure_at: Some(an_instant()),
            }],
        };
        let encoded = serde_json::to_string(&summary).expect("serialize the summary");
        let decoded: ScanSummaryView = serde_json::from_str(&encoded).expect("read it back");
        assert!((decoded.busy_seconds - 942.5).abs() < f64::EPSILON);
        assert_eq!(decoded.failures_open, 37);
        assert_eq!(decoded.providers[0].last_failure_at, Some(an_instant()));
    }

    /// The activity payload arrives over SSE, not through a typed client call, so serde is the
    /// only check on it. `target` is free-form JSON the tail reads two keys out of.
    #[test]
    fn a_live_activity_payload_round_trips() {
        let run_id = ScanRunId::new();
        let activity = ScanActivityView {
            runs: vec![RunActivityView {
                run_id,
                queued_tasks: 26,
                running_tasks: 4,
                oldest_claim_at: Some(an_instant()),
                kinds: vec!["series".to_owned()],
                workers: 2,
                stage: Some("series_chapters".to_owned()),
                stage_at: Some(an_instant()),
                stage_done: Some(12),
                stage_total: Some(94),
                stage_detail: Some("/manga/x".to_owned()),
                waiting_since: Some(an_instant()),
            }],
            events: vec![TaskEventView {
                id: Uuid::now_v7(),
                run_id,
                provider_slug: Some("kunmanga".to_owned()),
                kind: "series".to_owned(),
                state: TaskState::Failed,
                target: json!({ "path": "/manga/x", "page": 3 }),
                error: Some("http 503".to_owned()),
                attempts: 2,
                finished_at: Some(an_instant()),
            }],
        };
        let encoded = serde_json::to_string(&activity).expect("serialize the activity");
        let decoded: ScanActivityView = serde_json::from_str(&encoded).expect("read it back");
        assert_eq!(decoded.runs[0].oldest_claim_at, Some(an_instant()));
        assert_eq!(decoded.runs[0].workers, 2);
        assert_eq!(decoded.events[0].state, TaskState::Failed);
        assert_eq!(decoded.events[0].target["page"], 3);
    }

    /// A failure that recorded no error is a group the feed shows and an operator can clear, so
    /// its `null` has to survive as `null`, not an empty string.
    #[test]
    fn a_failure_and_its_group_round_trip_including_the_null_error() {
        let group = FailureGroupView {
            error: None,
            count: 12,
            cleared: 5,
            providers: vec!["kunmanga".to_owned()],
            kinds: vec!["catalog_page".to_owned()],
            latest_at: Some(an_instant()),
        };
        let encoded = serde_json::to_string(&group).expect("serialize the group");
        let decoded: FailureGroupView = serde_json::from_str(&encoded).expect("read it back");
        assert_eq!(decoded.error, None, "the null-error group stays null");
        assert_eq!(decoded.cleared, 5);
        assert_eq!(decoded.kinds, vec!["catalog_page".to_owned()]);

        let failure = FailedTaskView {
            id: Uuid::now_v7(),
            run_id: Uuid::now_v7(),
            provider_slug: None,
            mode: "full".to_owned(),
            kind: "series".to_owned(),
            error: Some("selector missing".to_owned()),
            attempts: 3,
            finished_at: Some(an_instant()),
            acknowledged_at: Some(an_instant()),
        };
        let encoded = serde_json::to_string(&failure).expect("serialize the failure");
        let decoded: FailedTaskView = serde_json::from_str(&encoded).expect("read it back");
        assert_eq!(decoded.acknowledged_at, Some(an_instant()));
        assert_eq!(
            decoded.provider_slug, None,
            "a deleted provider leaves the failure in the feed with no slug"
        );

        let cleared = FailuresClearedView { cleared: 37 };
        let encoded = serde_json::to_string(&cleared).expect("serialize the count");
        let decoded: FailuresClearedView = serde_json::from_str(&encoded).expect("read it back");
        assert_eq!(decoded.cleared, 37);
    }

    /// An empty clear body must mean "the whole feed", not "the group with no error".
    ///
    /// `match_null_error` defaults to `false` when absent, or every unqualified clear would
    /// narrow to one group. An omitted and a `null` `error` decode the same, so the flag alone
    /// tells the whole feed from the null-error group.
    #[test]
    fn an_empty_clear_body_selects_the_whole_feed() {
        let body = ClearFailuresBody::default();
        assert!(!body.match_null_error);

        let encoded = serde_json::to_value(&body).expect("serialize an empty body");
        assert_eq!(
            encoded,
            json!({ "match_null_error": false }),
            "only the flag is on the wire"
        );

        let decoded: ClearFailuresBody = serde_json::from_str("{}").expect("read an empty body");
        assert_eq!(decoded.provider, None);
        assert_eq!(decoded.since, None);
        assert_eq!(decoded.error, None);
        assert!(!decoded.match_null_error);
    }

    /// The null-error group is requested by the flag alone, and a named group carries its text.
    #[test]
    fn a_clear_body_distinguishes_the_null_group_from_a_named_one() {
        let null_group = ClearFailuresBody {
            match_null_error: true,
            ..ClearFailuresBody::default()
        };
        let encoded = serde_json::to_value(&null_group).expect("serialize the null group");
        assert_eq!(encoded, json!({ "match_null_error": true }));

        let named = ClearFailuresBody {
            provider: Some("kunmanga".to_owned()),
            error: Some("http 503".to_owned()),
            ..ClearFailuresBody::default()
        };
        let encoded = serde_json::to_value(&named).expect("serialize a named group");
        assert_eq!(
            encoded,
            json!({
                "provider": "kunmanga",
                "error": "http 503",
                "match_null_error": false,
            })
        );
    }
}
