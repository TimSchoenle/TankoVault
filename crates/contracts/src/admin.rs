//! Operator-console response bodies for `/v1/admin/*`.
//!
//! Converted from `tankovault-db` rows in `services/api` rather than derived directly from
//! them, so a `SELECT` column rename is a compile error instead of a silent public-API change.
//! Types pin their `OpenAPI` component name with `#[schema(as = ...)]` where it differs from the
//! Rust name, since `crates/api-client` and the frontend are generated from those names.

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use tankovault_domain::{
    AccountStatus, ProviderId, RunState, ScanMode, ScanRunId, SeriesId, TaskState,
};
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

/// System-wide rollup for the console header.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = SystemStats)]
pub struct SystemStatsView {
    /// Every provider row, whatever its state.
    pub providers_total: i64,
    /// Providers in `active`.
    pub providers_active: i64,
    /// Providers an operator turned off.
    pub providers_disabled: i64,
    /// Providers in a non-serving health state (degraded/challenged/solving/blocked).
    pub providers_unhealthy: i64,
    /// Canonical series, not provider pages.
    pub series_total: i64,
    /// Series-to-provider links across every provider.
    pub sources_total: i64,
    /// Chapter links held, part releases counted separately.
    pub chapters_total: i64,
    /// Chapters first seen in the last hour, by discovery rather than publication date.
    pub chapters_1h: i64,
    /// Chapters first seen in the last 24 hours.
    pub chapters_24h: i64,
    /// Chapters first seen in the last 7 days.
    pub chapters_7d: i64,
    /// Registered accounts, suspended ones included.
    pub users_total: i64,
    /// Merge candidates nobody has resolved yet.
    pub pending_merges: i64,
    /// Scan runs currently queued or running.
    pub runs_active: i64,
    /// The subset of `runs_active` that has actually started.
    pub runs_running: i64,
    /// Tasks waiting for a worker across every run.
    pub tasks_queued: i64,
    /// Tasks claimed or fetching.
    pub tasks_running: i64,
    /// Tasks that failed in the last 24 hours, by the time they settled.
    pub tasks_failed_24h: i64,
}

/// One row of the per-provider statistics table. Enum columns are text-cast; the provider's
/// identity fields are joined in so the console renders the table from one fetch.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = ProviderStat)]
pub struct ProviderStatView {
    /// The provider these figures are for.
    pub provider_id: Uuid,
    /// Its slug, joined in so the table renders from one fetch.
    pub slug: String,
    /// Its display name.
    pub name: String,
    /// Provider health state token (`active` | `disabled` | `blocked` | …).
    pub state: String,
    /// Adapter implementation token (`madara` | `generic_config` | `custom`).
    pub adapter: String,
    /// Distinct series that have at least one source on this provider.
    pub series_count: i64,
    /// Source links (series ↔ provider joins) this provider owns.
    pub source_count: i64,
    /// Source links currently in a non-active state.
    pub blocked_sources: i64,
    /// Chapter links under this provider's sources.
    pub chapter_count: i64,
    /// Chapters first seen here in the last 24 hours.
    pub chapters_24h: i64,
    /// Chapters first seen here in the last 7 days.
    pub chapters_7d: i64,
    /// When a chapter was last discovered here, `null` for a provider with none.
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub last_chapter_at: Option<OffsetDateTime>,
    /// The most recent scan across this provider's sources, `null` until one finishes.
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub last_scanned_at: Option<OffsetDateTime>,
    /// When an archive rebuild last completed, `null` if none ever has.
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub last_full_scan_at: Option<OffsetDateTime>,
    /// State of the provider's most recent scan run, if any.
    pub last_run_state: Option<String>,
    /// When the most recent run was created, `null` for a provider never scanned.
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub last_run_at: Option<OffsetDateTime>,
}

/// One privileged-action record enriched with the actor's username, for the console feed.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AuditView {
    /// The audit row.
    pub id: Uuid,
    /// Actor username (`None` for system-originated actions or a since-deleted user).
    pub actor: Option<String>,
    /// What was done, as a stable action token.
    pub action: String,
    /// What it was done to, `null` for an action with no single subject.
    pub target: Option<String>,
    /// Action-specific detail. Shape belongs to the action, not to this type.
    pub detail: Json,
    /// When the action was recorded.
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub created_at: OffsetDateTime,
}

/// A scan run as the console reads it: the persisted row plus the slug of the provider it was
/// scoped to.
///
/// Published under the `ScanRun` component name, which is what the generated client and the
/// frontend already call it. The slug is the addition: a run carrying only `provider_id` renders
/// as a truncated uuid and cannot be matched against a filter an operator typed, which is why
/// the panel's provider filter had no effect on anything the live stream pushed.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = ScanRun)]
pub struct ScanRunView {
    /// The run.
    pub id: ScanRunId,
    /// `null` for a run covering every enabled provider, and once the named provider is
    /// deleted.
    pub provider_id: Option<ProviderId>,
    /// `null` for an all-provider run, and for a run whose provider has since been deleted.
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
    /// Total matching the current filter, ignoring `limit`/`offset`.
    pub total: i64,
}

/// What the scan filter matched, as figures rather than rows.
///
/// Scoped by the *same* provider and window the row list uses, so a narrowed filter reports its
/// own success rate. Rates are left to the reader to divide: publishing `tasks_done` and
/// `tasks_total` rather than a percentage keeps the panel able to show both the ratio and the
/// magnitude behind it, which a lone percentage cannot.
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
    /// Of those, finished with every task failed, which is what marks a run failed rather
    /// than completed.
    pub runs_failed: i64,
    /// Of those, stopped by an operator.
    pub runs_cancelled: i64,
    /// Tasks those runs dispatched.
    pub tasks_total: i64,
    /// Of those, settled successfully. The numerator the panel divides itself.
    pub tasks_done: i64,
    /// Of those, settled as failures.
    pub tasks_failed: i64,
    /// Failures still in the triage feed, as opposed to `tasks_failed`, which counts every
    /// failure in the window including the ones an operator has cleared.
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
/// Run counters alone cannot distinguish "working" from "wedged" — both leave `done_tasks`
/// where it was. These are the figures that can: what a worker is holding, since when, and what
/// settled most recently.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ScanActivityView {
    /// One entry per run in flight, and none for a run that has settled.
    pub runs: Vec<RunActivityView>,
    /// The most recently settled tasks of the runs in flight, newest first. Empty when nothing
    /// is running, which is the honest answer rather than a replay of the last scan.
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
    /// What the run's oldest held task is doing right now — a
    /// [`tankovault_domain::ScanStage`] token. `null` when nothing is held, which paired with
    /// `waiting_since` is how the console tells "working" from "waiting for a worker".
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
    /// What the stage is working against — a series path, a catalogue page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage_detail: Option<String>,
    /// When the run's oldest still-queued task was created.
    ///
    /// With `running_tasks` at zero this is the run's real state: not working, **waiting for a
    /// worker slot**. A worker runs one task per provider, so a provider's second run queues
    /// behind its first — and through `state` alone that is indistinguishable from a run that
    /// has hung, which is exactly the reading it used to get.
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

/// Which in-flight runs a bulk cancellation stops. Both fields narrow; a body with neither stops
/// everything currently queued or running.
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
    /// Summed milliseconds per stage across the run, largest first — the answer to "what was this
    /// run actually doing for twenty minutes".
    pub stages: Vec<StageTotalView>,
    /// Every task of the run, whatever its state.
    pub tasks: Vec<ScanTaskDetailView>,
}

/// A run's time, summed over every task that recorded a breakdown.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RunTelemetryView {
    /// Settled tasks carrying a breakdown. Everything else here sums over exactly these, so a run
    /// from before this instrumentation reports zero rather than a wrong total.
    pub tasks_measured: i64,
    /// Summed execution time in milliseconds. Exceeds the run's wall clock when tasks ran
    /// concurrently: it is work performed, not time elapsed.
    pub busy_ms: i64,
    /// Summed time tasks spent created but unclaimed — queued behind a busy provider.
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
    /// Responses the provider answered 429/503, each of which widened its spacing thereafter.
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
    /// The stage the task is in, or the one it ended in. On a failure this is the most useful
    /// field on the row: it says whether the provider stopped answering or our own ingest
    /// rejected what it sent.
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
    /// `null` for tasks created before the column existed.
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
    /// One error group, exactly as the grouped feed reported it. Send `error` with a `null`
    /// value together with `match_null_error` to clear the group that recorded no error at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Clear the group whose error is absent. Mutually meaningful with `error`: an omitted
    /// `error` means "any error", which is not the same request.
    #[serde(default)]
    pub match_null_error: bool,
}

/// How many failures a clear actually hid.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FailuresClearedView {
    /// Rows this call acknowledged. Already-cleared rows are excluded, so two operators clearing
    /// the same feed do not both claim it.
    pub cleared: i64,
}

/// One distinct scan failure, with how often it happened and which providers it hit.
///
/// The grouped view of the failure feed: twelve rows of the same broken selector are one
/// problem, and the flat feed presents them as twelve.
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
    /// Task kinds this error struck, sorted — the same message on a `series` task and on a
    /// `catalog_page` task is two problems, not one.
    pub kinds: Vec<String>,
    /// The most recent of them, `null` only if the group is empty.
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub latest_at: Option<OffsetDateTime>,
}

/// A page of the audit trail plus how many records the filter matches in total.
///
/// An envelope rather than a bare list, because the trail is deep enough that the console can
/// only ever hold a window on it, and a window with no total is a pager that cannot say where
/// it is.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AuditPageView {
    /// The page, newest record first.
    pub items: Vec<AuditView>,
    /// Total matching the current filter, ignoring `limit`/`offset`.
    pub total: i64,
}

/// A failed scan task enriched with its run's provider + mode, for the console error feed.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FailedTaskView {
    /// The failed task.
    pub id: Uuid,
    /// The run it belonged to.
    pub run_id: Uuid,
    /// Provider slug of the owning run (`None` if the provider was since deleted).
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

/// A pending merge candidate, enriched with everything the console needs to triage it without
/// opening both series (design §11 `GET /v1/admin/merge-candidates`).
///
/// The counts and `suggested_keep` are not decoration. Acting on a candidate means choosing
/// which of the two rows survives, and the absorbed id *stops existing* — every bookmark,
/// notification and external tracker mapping naming it breaks. The right answer is whichever
/// series carries more of the catalogue, which is a comparison the server can make once instead
/// of an operator eyeballing it per row.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MergeCandidateView {
    /// The queue row, which is what a resolve call names.
    pub id: Uuid,
    /// One side of the pair.
    pub series_id: SeriesId,
    /// Its canonical title.
    pub series_title: String,
    /// Providers carrying that side.
    pub series_sources: i64,
    /// Chapter links under that side.
    pub series_chapters: i64,
    /// The other side.
    pub candidate_id: SeriesId,
    /// Its canonical title.
    pub candidate_title: String,
    /// Providers carrying that side.
    pub candidate_sources: i64,
    /// Chapter links under that side.
    pub candidate_chapters: i64,
    /// Similarity in `[0,1]` after every scoring term.
    pub score: f32,
    /// Stable slugs for the scoring rules that fired — `exact_title`, `compact_identity`,
    /// `alias_identity`, `near_identical`, `shared_author`, and so on. Rendered as badges; the
    /// set is `tankovault_domain::matching::MatchSignals::labels`.
    pub signals: Vec<String>,
    /// The rule that put the pair here, `null` for a row from before the journal existed.
    pub reason: Option<String>,
    /// Which side the console should offer to keep. Advisory: the merge endpoint takes an
    /// explicit direction.
    pub suggested_keep: SeriesId,
    /// When the pair first entered the queue.
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub created_at: OffsetDateTime,
    /// When a sweep last re-scored the row in place.
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub updated_at: OffsetDateTime,
}

/// Which kind of recommendation-model build to run.
///
/// Lives here rather than in either service because both sides of the internal hop parse it: the
/// API validates the operator's request body and the control plane acts on it. A hand-mirrored
/// second copy is the drift this crate exists to prevent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RecsysBuildMode {
    /// Patch the live generation: the repair queue, then whatever it has not reached.
    Incremental,
    /// Re-solve the projection basis and re-embed the catalogue. What a `next_full_build` tuning
    /// change needs before it means anything.
    Full,
}

/// What one recommendation-model build request did.
///
/// Returned by `POST /v1/admin/recommendations/rebuild`, produced by the control plane that
/// actually runs the build.
///
/// The build runs detached, so this answers only whether one was *started*: a build takes
/// minutes to hours and no request may be held open for it. What it went on to do — stage,
/// progress, counts, and how it ended — is on `GET /v1/admin/recommendations/health`.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, ToSchema)]
pub struct RecsysBuildView {
    /// `false` when another build already held the claim. The correct response is to wait for
    /// it, not to retry: the other build is doing this one's work, and `generation` is zero
    /// because this call started nothing.
    pub started: bool,
    /// The generation the build claimed and will write under.
    pub generation: i32,
}

/// Whether an exhaustive duplicate sweep was started.
///
/// Returned by `POST /v1/admin/merge-candidates/sweep-all`. The run is detached, so this answers
/// only whether one *started*: it draws rounds until every shortlist has been walked out, which
/// is far longer than a request may be held open for. What the run goes on to do — rounds,
/// counters and how it ended — is on `GET /v1/admin/merge-candidates/sweep-all`.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, ToSchema)]
pub struct MergeFullSweepView {
    /// `false` when a live run already held the claim. The correct response is to watch that
    /// run, not to retry: it is doing this one's work, and starting a second would spend the
    /// automatic-merge ceiling twice over.
    pub started: bool,
}

/// What one duplicate-reconciliation sweep did.
///
/// Returned by `POST /v1/admin/merge-candidates/sweep` and logged by the scheduled sweep, so an
/// operator can see the effect of a threshold change on a single run before leaving it to the
/// schedule.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, ToSchema)]
pub struct MergeSweepView {
    /// Pairs re-scored. The shortlist is produced by blocking on the whitespace-insensitive
    /// title key, so this is far smaller than the number of series.
    pub pairs_examined: i64,
    /// Pairs merged without asking, because a structural identity rule fired *and* the score
    /// cleared the automatic-merge threshold.
    pub auto_merged: i64,
    /// Pairs the sweep put in the review queue that had never been there.
    ///
    /// Counted apart from `requeued` because only these lengthen the queue: the sweep re-scores
    /// rows that are already open on every run, and folding those into one "queued" number
    /// reports a queue growing by hundreds when it grew by tens. Net change in queue length is
    /// `queued + reopened - withdrawn`.
    pub queued: i64,
    /// Rows already open that this sweep re-scored in place. The queue is no longer for these.
    pub requeued: i64,
    /// Pairs the scorer had closed as distinct that enrichment has brought back into review.
    pub reopened: i64,
    /// Open queue rows removed because re-scoring with everything now known about both series
    /// put the pair below the review floor.
    pub withdrawn: i64,
    /// Pairs the sweep judged distinct and left alone.
    pub distinct: i64,
    /// Pairs skipped because the sweep's per-run automatic-merge budget was exhausted. Non-zero
    /// means the next sweep has more to do, not that anything failed.
    pub deferred: i64,
    /// Pairs an auto-merge guard held back: identical titles, a score above the automatic
    /// threshold, and a signal saying the two are different works anyway. These are the near
    /// misses, and they are the rows to read first — each is either a duplicate the guard is
    /// costing you or a merge the guard just prevented.
    pub blocked: i64,
    /// Pairs skipped because a merge earlier in this same run absorbed one of their two series.
    ///
    /// Their facts were loaded before that merge and are now stale, so re-judging them here would
    /// score the survivor as it was rather than as it is. A three-way duplicate therefore takes
    /// more than one pass; non-zero means another pass has something to do, and the scheduler
    /// runs one rather than leaving the chain until the next tick.
    pub chains_deferred: i64,
}

/// One knob of the automatic-merge policy, as the console shows it.
///
/// Assembled by the control plane rather than by the API, because the effective value is the
/// stored override *layered over this deployment's configured* `matching` block — and the
/// control plane is the service that holds both. An API that resolved it against the compiled
/// registry instead would report a default the sweep does not use.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = MergePolicyKnob)]
pub struct MergePolicyView {
    /// The persisted key, e.g. `matching.auto_merge`.
    pub key: String,
    /// Section-local label.
    pub title: String,
    /// What the value does, written to be read immediately before someone changes production.
    pub description: String,
    // Tokens rather than the domain enums: utoipa publishes a `///` as the public description,
    // and a rustdoc link to a crate the client cannot see is noise in a generated client.
    /// `ratio` for the threshold, `toggle` for a guard — what the console renders, a field or a
    /// switch.
    pub kind: String,
    /// When a change reaches the sweep. Every knob here is `next_sweep`.
    pub applies: String,
    /// The effective value, already clamped: exactly what the next sweep will apply.
    pub value: f64,
    /// What this deployment falls back to when no override is stored — its configured
    /// `matching` value, which is the compiled default unless the deployment set it. This is
    /// what resetting the knob returns it to, so it is not the same number as the compiled
    /// default and must not be shown as one.
    pub default_value: f64,
    /// Inclusive bounds. Enforced by the control plane, not only by the UI.
    pub min: f64,
    /// Inclusive upper bound.
    pub max: f64,
    /// Whether an operator has explicitly decided this one, as opposed to it following the
    /// deployment's configuration.
    pub overridden: bool,
    /// Why it was last changed, if the operator said.
    pub note: Option<String>,
    /// Username of the operator who last changed it; `None` once that account is erased.
    pub updated_by: Option<String>,
    /// RFC 3339. Absent while the knob follows the configuration.
    pub updated_at: Option<String>,
}

/// What a normalized-key rebuild changed.
///
/// `normalized_title` is a persisted matching key, so a change to the normalization rules only
/// reaches rows that happen to be re-scanned until this runs.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, ToSchema)]
pub struct KeyRebuildView {
    /// Canonical titles read.
    pub series_scanned: i64,
    /// Of those, whose key the corrected rules changed.
    pub series_updated: i64,
    /// Alternative titles read.
    pub titles_scanned: i64,
    /// Of those, whose key the corrected rules changed.
    pub titles_updated: i64,
    /// Alternative titles dropped because the corrected rules collapsed them onto a key the
    /// same series already held.
    pub titles_deduplicated: i64,
}

/// One row of the admin Sync console's "Linked accounts" table.
///
/// The automatic-sync policy columns and the pending-conflict count (design v2 §B.7) are
/// visibility only. They are the user's settings, and no operator route writes them.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = AdminAccountRow)]
pub struct SyncAccountView {
    /// The local account the link belongs to.
    pub user_id: Uuid,
    /// Its username, joined in so the table renders from one fetch.
    pub username: String,
    /// Which external tracker, as a slug.
    pub provider: String,
    /// The handle on that tracker, `null` until a sync has read it.
    pub external_username: Option<String>,
    /// When a sync last completed for this link, `null` if none has.
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub last_synced_at: Option<OffsetDateTime>,
    /// Why the most recent sync failed, `null` when the last one succeeded.
    pub last_error: Option<String>,
    /// The user's own setting for scheduled syncing. Read-only to an operator.
    pub auto_sync_enabled: bool,
    /// The user's own answer to a two-sided change: which side wins.
    pub conflict_policy: String,
    /// Conflicts on this link nobody has resolved.
    pub pending_conflicts: i64,
    /// When the link was made.
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub created_at: OffsetDateTime,
}

/// One row of the admin Sync console's "Series mappings" table.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = AdminMappingRow)]
pub struct SyncMappingView {
    /// The local series.
    pub series_id: Uuid,
    /// Its canonical title.
    pub series_title: String,
    /// Which external tracker, as a slug.
    pub provider: String,
    /// The tracker's own id for the same work.
    pub external_id: String,
    /// When the mapping was last written.
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub updated_at: OffsetDateTime,
}

/// One row of the admin Sync console's "Assign queue" — a canonical series that has **no**
/// external mapping for the given provider yet, so an operator can review and assign one.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = UnmappedSeriesRow)]
pub struct UnmappedSeriesView {
    /// The unmapped series.
    pub series_id: Uuid,
    /// Its canonical title.
    pub series_title: String,
    /// How many local sources back this series (a proxy for how confident a match is worth).
    pub source_count: i64,
}

/// One row of the admin console's "Unmatched remote entries" queue: a fetched provider entry
/// the auto-matcher could not confidently link to a local series.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = RemoteEntryRow)]
pub struct RemoteEntryView {
    /// Whose list the entry came off.
    pub user_id: Uuid,
    /// That user's username.
    pub username: String,
    /// Which external tracker, as a slug.
    pub provider: String,
    /// The tracker's own id for the entry.
    pub external_id: String,
    /// Title as the tracker spells it, which is what the operator matches on.
    pub title: String,
    /// Tracking status as the tracker spells it.
    pub status: String,
    /// Chapters read, on the tracker's scale.
    pub progress: f64,
    /// Medium as the tracker spells it.
    pub content_type: String,
    /// Year the tracker gives, `null` when it gives none.
    pub start_year: Option<i32>,
}

/// One row of the operator user directory.
///
/// Carries a grant *count* rather than the grants themselves: the directory is a list, and a
/// user's actual capabilities are what the detail view is for. The count is enough to answer
/// "which of these accounts are privileged at all", which is the question the list is scanned
/// for.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = DirectoryRow)]
pub struct UserDirectoryRow {
    /// The account.
    #[schema(value_type = String)]
    pub id: Uuid,
    /// Its sign-in address.
    pub email: String,
    /// Its display handle.
    pub username: String,
    /// Whether it may authenticate at all.
    pub status: AccountStatus,
    /// Whether the address has been confirmed. An unverified account that has existed for
    /// months is usually an abandoned registration.
    pub email_verified: bool,
    /// How many permissions this account holds. `0` is an ordinary reader.
    ///
    /// Not a measure of *how much* an account may do: the super user holds one grant and can do
    /// everything, which is what `is_super_user` exists to say.
    pub permission_count: i64,
    /// Whether this account holds the super-user grant.
    ///
    /// Published separately because that grant is deliberately absent from the permission
    /// catalogue, so a client reconciling grants against the catalogue would otherwise render
    /// the deployment owner as an account holding nothing.
    pub is_super_user: bool,
    /// How many series the user tracks — the cheapest signal of a real, in-use account.
    pub tracked_count: i64,
    /// Its most recent sign-in, `null` for an account that has never signed in.
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub last_login_at: Option<OffsetDateTime>,
    /// When it was registered.
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub created_at: OffsetDateTime,
}

/// A page of the directory plus the unfiltered-by-page total, so the UI can render
/// "showing 1–25 of 312" without a second request.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = DirectoryPage)]
pub struct UserDirectoryPage {
    /// The page, in the directory's own order.
    pub users: Vec<UserDirectoryRow>,
    /// Total matching the current search, ignoring `limit`/`offset`.
    pub total: i64,
}

/// Everything the user-detail panel shows, minus the grant list (fetched separately by
/// `tankovault_db::repo::permissions::list_for_user` so the panel can refresh just that part
/// after an edit).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = UserDetail)]
pub struct UserDetailView {
    /// The account.
    #[schema(value_type = String)]
    pub id: Uuid,
    /// Its sign-in address.
    pub email: String,
    /// Its display handle.
    pub username: String,
    /// Whether it may authenticate at all.
    pub status: AccountStatus,
    /// Whether the address has been confirmed.
    pub email_verified: bool,
    /// What an operator gave when suspending, `null` while the account is active.
    pub suspension_reason: Option<String>,
    /// When it was suspended, `null` while it is active.
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub suspended_at: Option<OffsetDateTime>,
    /// Its most recent sign-in, `null` for an account that has never signed in.
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub last_login_at: Option<OffsetDateTime>,
    /// When it was registered.
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub created_at: OffsetDateTime,
    /// Live login sessions. Tells an operator whether a suspension will actually take effect
    /// without also revoking sessions.
    pub active_sessions: i64,
    /// Series the user tracks.
    pub tracked_count: i64,
    /// Linked external trackers, so an operator can see the account has third-party
    /// credentials at rest before deciding to erase it.
    pub linked_accounts: i64,
    /// Unresolved data-subject requests filed by this user. An account with an open erasure
    /// request must not be quietly edited.
    pub open_privacy_requests: i64,
}

/// A single grant, with its provenance, for the user-detail view.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = GrantRow)]
pub struct GrantView {
    /// The permission token. A string rather than the enum because a row surviving from a
    /// build that had a capability this one does not must still be *visible* to an
    /// administrator — that is precisely when they need to see and remove it.
    pub permission: String,
    /// Whether this build recognises the token. `false` means the grant is inert.
    pub known: bool,
    /// When the grant was made.
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub granted_at: OffsetDateTime,
    /// Who granted it, or `None` for a grant made by the migration from the old role model
    /// or by an administrator since erased.
    pub granted_by: Option<String>,
}

/// A queue entry as the operator sees it: the subject's identity (while they still exist) and
/// who is handling it.
///
/// The subject-facing fields are a nested `request` rather than `#[serde(flatten)]`. Flattening
/// reads better on the wire, but `utoipa` cannot describe it: a flattened field contributes no
/// properties to the generated schema, so the typed client ended up with a struct missing every
/// field the queue actually renders. A nested object is honest about its shape and survives
/// code generation.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = AdminRequestRow)]
pub struct AdminPrivacyRequestView {
    /// The subject-facing record, exactly as the subject sees it on their own page.
    pub request: crate::me::PrivacyRequestView,
    /// The subject's id, or `None` once they have been erased.
    #[schema(value_type = Option<String>)]
    pub user_id: Option<Uuid>,
    /// The subject's username. `None` means the account is gone — for a completed erasure
    /// that is the expected end state, not missing data.
    pub username: Option<String>,
    /// The subject's address, `null` once they have been erased.
    pub email: Option<String>,
    /// Operator who claimed it, if any.
    pub claimed_by: Option<String>,
    /// Operator who resolved it, if resolved.
    pub resolved_by: Option<String>,
    /// Whether the Art. 12(3) deadline has passed with the request still open. Computed in
    /// SQL against `now()` so the queue cannot disagree with itself about what is late
    /// depending on when a client's clock says it rendered.
    pub overdue: bool,
    /// Whether fulfilling this request means disclosing the subject's data export, i.e.
    /// whether the queue should offer the "release export" action on this row.
    ///
    /// Derived server-side from `RequestKind::needs_export` for the same reason `overdue` is
    /// computed in SQL: the console used to re-derive it from `kind` in its own `match`
    /// (FRONTEND F10), so the set of kinds that disclose an export lived in two places with
    /// nothing connecting them — and the one the operator sees is the one that decides which
    /// button appears. The server refuses the call either way, so the divergence would have
    /// shown as a button that does nothing rather than as a disclosure; still a bug, and one
    /// no test could reach.
    pub needs_export: bool,
}

/// What the control plane answers when a scan is planned: the runs it created.
///
/// Published by `services/control-plane` on its internal `POST /internal/scans`, and
/// republished verbatim by `services/api` on `POST /v1/admin/scans` and
/// `POST /v1/admin/providers/{id}/resolve`. It lives here, rather than staying a private
/// struct in the control plane's `main.rs`, for the reason ARCH-10 exists: while the producer
/// owned the only definition, the API could declare nothing more specific than
/// `serde_json::Value`, so the console's "N scans queued" was reading a field no compiler had
/// ever connected to the field the planner writes. Both ends now name this type, so removing
/// `run_ids` fails to build at the producer *and* the republisher.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = ScanTriggered)]
pub struct ScanTriggeredView {
    /// One id per provider scanned — one per active provider when the request names none. A
    /// provider already scanning in this mode contributes the id of that run rather than a new
    /// one, so an id here means "this run covers your request", not always "this run is new".
    pub run_ids: Vec<ScanRunId>,
}

/// One row of the automatic-merge decision journal.
///
/// The journal answers the question a score cannot: *why*. `terms` itemises how the number was
/// reached, `reason` names the rule that turned it into a verdict, `blocked_by` names the guards
/// that overrode it, and `evidence` carries both sides' facts and which title actually matched.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = MergeDecision)]
pub struct MergeDecisionView {
    /// The journal row, which is what a revert or a flag names.
    pub id: Uuid,
    /// When the verdict was taken.
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub decided_at: OffsetDateTime,
    /// Groups every decision of one sweep run; absent for an operator's console merge.
    pub sweep_id: Option<Uuid>,
    /// `sweep_new` | `sweep_requeue` | `sweep_recheck` | `operator`.
    pub trigger: String,
    /// The operator behind an `operator` trigger, `null` for a sweep.
    pub actor: Option<Uuid>,
    /// One side of the pair, as it was at decision time.
    pub left_id: SeriesId,
    /// The other side.
    pub right_id: SeriesId,
    /// Left title as it read then, kept even after the series is absorbed.
    pub left_title: String,
    /// Right title as it read then.
    pub right_title: String,
    /// `auto` | `review` | `distinct`.
    pub verdict: String,
    /// The stable slug of the rule that produced the verdict.
    pub reason: String,
    /// Guards that fired. Non-empty on a `review` row means the pair cleared the score and
    /// identity bar and was held back anyway.
    pub blocked_by: Vec<String>,
    /// What was actually done, which is not always the verdict: `merged`, `queued`, `requeued`,
    /// `reopened`, `withdrawn`, `distinct`, `deferred`.
    pub outcome: String,
    /// The series that survived, `null` for anything but a merge.
    pub survivor_id: Option<SeriesId>,
    /// The series that stopped existing, `null` for anything but a merge.
    pub absorbed_id: Option<SeriesId>,
    /// The final similarity in `[0,1]`, after every term in `terms`.
    pub score: f32,
    /// The similarity the score started from, before any bonus or penalty.
    pub base_score: f32,
    /// Stable slugs for the scoring rules that fired.
    pub signals: Vec<String>,
    /// `[{rule, delta, detail}]` — every term the scorer applied, in order.
    pub terms: Json,
    /// Both sides' facts, which titles matched, and how the survivor was chosen.
    pub evidence: Json,
    /// The thresholds and guards in force when the decision was taken.
    pub policy: Json,
    /// Whether this decision still has an unspent undo journal.
    pub revertible: bool,
    /// How many rows a revert would restore or move back.
    pub undo_rows: i64,
    /// When the merge was undone, `null` while it stands.
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub reverted_at: Option<OffsetDateTime>,
    /// Who undid it.
    pub reverted_by: Option<Uuid>,
    /// What they gave as the reason.
    pub revert_reason: Option<String>,
    /// When an operator marked the decision wrong, `null` if nobody has.
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub flagged_at: Option<OffsetDateTime>,
    /// Who marked it.
    pub flagged_by: Option<Uuid>,
    /// What they gave as the reason.
    pub flag_reason: Option<String>,
}

/// What undoing an automatic merge put back.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = MergeReverted)]
pub struct MergeRevertedView {
    /// The journal row that was undone.
    pub decision_id: Uuid,
    /// The series that exists again, under its original id.
    pub restored_id: SeriesId,
    /// The series the rows went back off.
    pub survivor_id: SeriesId,
    /// Rows restored or moved back off the survivor.
    pub rows_restored: i64,
    /// Always true: a revert also suppresses the pair, or the next sweep would merge it again.
    pub pair_suppressed: bool,
}

/// One row of the automatic-sync decision journal.
///
/// Covers what `sync_history` never did: the entries that matched no local series, the series
/// skipped as excluded, the fields both sides already agreed on, and the scored title match
/// behind every mapping.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = SyncDecision)]
pub struct SyncDecisionView {
    /// The journal row, which is what a revert or a flag names.
    pub id: Uuid,
    /// Groups one account reconciliation.
    pub run_id: Uuid,
    /// When the decision was taken.
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub decided_at: OffsetDateTime,
    /// Whose account was being reconciled.
    pub user_id: Uuid,
    /// That user's username, `null` once the account is erased.
    pub username: Option<String>,
    /// The local series, `null` for an entry that matched none.
    pub series_id: Option<SeriesId>,
    /// Its canonical title, `null` for the same reason.
    pub series_title: Option<String>,
    /// Which external tracker, as a slug.
    pub provider: String,
    /// The tracker's own id, `null` for a decision about a local series only.
    pub external_id: Option<String>,
    /// `match` | `progress` | `status` | `series` | `metadata`.
    pub scope: String,
    /// `matched` | `unmatched` | `pull` | `push` | `create_remote` | `conflict` | `noop`
    /// | `skipped` | `import_status` | `enriched` | `unmapped`.
    pub action: String,
    /// The stable slug for why: `only_remote_changed`, `both_sides_changed_policy_remote_wins`,
    /// `title_match_above_threshold`, `blocked_by_operator`, …
    pub reason: String,
    /// The conflict policy in force, `null` where the decision needed none.
    pub policy: Option<String>,
    /// Whether anything was actually written. A run is mostly considerations.
    pub applied: bool,
    /// The local value before the decision.
    pub local_before: Option<String>,
    /// The local value after it, equal to `local_before` when nothing was written.
    pub local_after: Option<String>,
    /// The remote value before the decision.
    pub remote_before: Option<String>,
    /// The remote value after it.
    pub remote_after: Option<String>,
    /// The three-way merge's common ancestor. Without it a pull cannot be told from a clobber.
    pub ancestor_local: Option<String>,
    /// The remote side of that ancestor.
    pub ancestor_remote: Option<String>,
    /// Title-match similarity in `[0,1]`, `null` outside a match decision.
    pub match_score: Option<f32>,
    /// Stable slugs for the matching rules that fired. Empty outside a match decision.
    pub match_signals: Vec<String>,
    /// Which titles matched, the scored terms, the runner-up, and the provider's own metadata.
    pub evidence: Json,
    /// When the decision was undone, `null` while it stands.
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub reverted_at: Option<OffsetDateTime>,
    /// Who undid it.
    pub reverted_by: Option<Uuid>,
    /// What they gave as the reason.
    pub revert_reason: Option<String>,
    /// When an operator marked the decision wrong, `null` if nobody has.
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub flagged_at: Option<OffsetDateTime>,
    /// Who marked it.
    pub flagged_by: Option<Uuid>,
    /// What they gave as the reason.
    pub flag_reason: Option<String>,
}

/// What undoing a sync decision put back.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = SyncReverted)]
pub struct SyncRevertedView {
    /// The journal row that was undone.
    pub decision_id: Uuid,
    /// `local_progress` | `local_status` | `watchlist_entry` | `remote_entry` | `match`.
    pub restored: String,
    /// The value the restored side now holds.
    pub value: Option<String>,
    /// Whether the revert also refused the title match permanently.
    pub blocked_match: bool,
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

    /// The run page is what the history table renders, and the stamps are the part worth
    /// pinning: they are `OffsetDateTime` behind an RFC 3339 serde attribute, which is the one
    /// mistake the type system here cannot catch. The two workspaces are related by nothing but
    /// `openapi.json`, so a field that does not survive its round trip is a value the operator
    /// silently never sees.
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

    /// The activity payload arrives over SSE rather than through a typed client call, so serde
    /// is the only thing checking it — and `target` is free-form JSON the tail reads two keys out
    /// of, which a stricter round trip would not preserve.
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

    /// A failure that recorded no error is a real group the feed shows and an operator can clear,
    /// so its `null` has to survive as `null` rather than collapsing into an empty string.
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
    /// Both halves are load-bearing. `match_null_error` has to default to `false` when the field
    /// is absent, or every unqualified clear silently narrows to one group. And `error` has to be
    /// omitted from the wire rather than sent as `null`, because the handler reads an absent
    /// `error` as "any error" — serialising the `None` would send a different request.
    #[test]
    fn an_empty_clear_body_selects_the_whole_feed() {
        let body = ClearFailuresBody::default();
        assert!(!body.match_null_error);

        let encoded = serde_json::to_value(&body).expect("serialize an empty body");
        assert_eq!(
            encoded,
            json!({ "match_null_error": false }),
            "only the flag is on the wire; a `null` error means something else"
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
