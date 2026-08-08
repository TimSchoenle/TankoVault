//! Operator-console response bodies for `/v1/admin/*`.
//!
//! Converted from `tankovault-db` rows in `services/api` rather than derived directly from
//! them, so a `SELECT` column rename is a compile error instead of a silent public-API change.
//! Types pin their `OpenAPI` component name with `#[schema(as = ...)]` where it differs from the
//! Rust name, since `crates/api-client` and the frontend are generated from those names.

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use tankovault_domain::{AccountStatus, ScanRun, ScanRunId, SeriesId};
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

/// System-wide rollup for the console header.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = SystemStats)]
pub struct SystemStatsView {
    pub providers_total: i64,
    pub providers_active: i64,
    pub providers_disabled: i64,
    /// Providers in a non-serving health state (degraded/challenged/solving/blocked).
    pub providers_unhealthy: i64,
    pub series_total: i64,
    pub sources_total: i64,
    pub chapters_total: i64,
    pub chapters_1h: i64,
    pub chapters_24h: i64,
    pub chapters_7d: i64,
    pub users_total: i64,
    pub pending_merges: i64,
    /// Scan runs currently queued or running.
    pub runs_active: i64,
    pub runs_running: i64,
    pub tasks_queued: i64,
    pub tasks_running: i64,
    pub tasks_failed_24h: i64,
}

/// One row of the per-provider statistics table. Enum columns are text-cast; the provider's
/// identity fields are joined in so the console renders the table from one fetch.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = ProviderStat)]
pub struct ProviderStatView {
    pub provider_id: Uuid,
    pub slug: String,
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
    pub chapter_count: i64,
    pub chapters_24h: i64,
    pub chapters_7d: i64,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub last_chapter_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub last_scanned_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub last_full_scan_at: Option<OffsetDateTime>,
    /// State of the provider's most recent scan run, if any.
    pub last_run_state: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub last_run_at: Option<OffsetDateTime>,
}

/// One privileged-action record enriched with the actor's username, for the console feed.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AuditView {
    pub id: Uuid,
    /// Actor username (`None` for system-originated actions or a since-deleted user).
    pub actor: Option<String>,
    pub action: String,
    pub target: Option<String>,
    pub detail: Json,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub created_at: OffsetDateTime,
}

/// A page of scan runs plus how many the filter matches in total.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ScanRunPageView {
    pub items: Vec<ScanRun>,
    /// Total matching the current filter, ignoring `limit`/`offset`.
    pub total: i64,
}

/// One distinct scan failure, with how often it happened and which providers it hit.
///
/// The grouped view of the failure feed: twelve rows of the same broken selector are one
/// problem, and the flat feed presents them as twelve.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FailureGroupView {
    /// The error text these failures share. `null` groups the failures that recorded none.
    pub error: Option<String>,
    pub count: i64,
    /// Provider slugs affected, sorted.
    pub providers: Vec<String>,
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
    pub items: Vec<AuditView>,
    /// Total matching the current filter, ignoring `limit`/`offset`.
    pub total: i64,
}

/// A failed scan task enriched with its run's provider + mode, for the console error feed.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FailedTaskView {
    pub id: Uuid,
    pub run_id: Uuid,
    /// Provider slug of the owning run (`None` if the provider was since deleted).
    pub provider_slug: Option<String>,
    pub mode: String,
    pub kind: String,
    pub error: Option<String>,
    pub attempts: i16,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub finished_at: Option<OffsetDateTime>,
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
    pub id: Uuid,
    pub series_id: SeriesId,
    pub series_title: String,
    pub series_sources: i64,
    pub series_chapters: i64,
    pub candidate_id: SeriesId,
    pub candidate_title: String,
    pub candidate_sources: i64,
    pub candidate_chapters: i64,
    pub score: f32,
    /// Stable slugs for the scoring rules that fired — `exact_title`, `compact_identity`,
    /// `alias_identity`, `near_identical`, `shared_author`, and so on. Rendered as badges; the
    /// set is `tankovault_domain::matching::MatchSignals::labels`.
    pub signals: Vec<String>,
    pub reason: Option<String>,
    /// Which side the console should offer to keep. Advisory: the merge endpoint takes an
    /// explicit direction.
    pub suggested_keep: SeriesId,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub created_at: OffsetDateTime,
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
}

/// What a normalized-key rebuild changed.
///
/// `normalized_title` is a persisted matching key, so a change to the normalization rules only
/// reaches rows that happen to be re-scanned until this runs.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, ToSchema)]
pub struct KeyRebuildView {
    pub series_scanned: i64,
    pub series_updated: i64,
    pub titles_scanned: i64,
    pub titles_updated: i64,
    /// Alternative titles dropped because the corrected rules collapsed them onto a key the
    /// same series already held.
    pub titles_deduplicated: i64,
}

/// One row of the admin Sync console's "Linked accounts" table. The automatic-sync policy
/// columns and pending-conflict count (design v2 §B.7) are read-only operator visibility —
/// they are user settings, never operator-overridable.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = AdminAccountRow)]
pub struct SyncAccountView {
    pub user_id: Uuid,
    pub username: String,
    pub provider: String,
    pub external_username: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub last_synced_at: Option<OffsetDateTime>,
    pub last_error: Option<String>,
    pub auto_sync_enabled: bool,
    pub conflict_policy: String,
    pub pending_conflicts: i64,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub created_at: OffsetDateTime,
}

/// One row of the admin Sync console's "Series mappings" table.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = AdminMappingRow)]
pub struct SyncMappingView {
    pub series_id: Uuid,
    pub series_title: String,
    pub provider: String,
    pub external_id: String,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub updated_at: OffsetDateTime,
}

/// One row of the admin Sync console's "Assign queue" — a canonical series that has **no**
/// external mapping for the given provider yet, so an operator can review and assign one.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = UnmappedSeriesRow)]
pub struct UnmappedSeriesView {
    pub series_id: Uuid,
    pub series_title: String,
    /// How many local sources back this series (a proxy for how confident a match is worth).
    pub source_count: i64,
}

/// One row of the admin console's "Unmatched remote entries" queue: a fetched provider entry
/// the auto-matcher could not confidently link to a local series.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = RemoteEntryRow)]
pub struct RemoteEntryView {
    pub user_id: Uuid,
    pub username: String,
    pub provider: String,
    pub external_id: String,
    pub title: String,
    pub status: String,
    pub progress: f64,
    pub content_type: String,
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
    #[schema(value_type = String)]
    pub id: Uuid,
    pub email: String,
    pub username: String,
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
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub last_login_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub created_at: OffsetDateTime,
}

/// A page of the directory plus the unfiltered-by-page total, so the UI can render
/// "showing 1–25 of 312" without a second request.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = DirectoryPage)]
pub struct UserDirectoryPage {
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
    #[schema(value_type = String)]
    pub id: Uuid,
    pub email: String,
    pub username: String,
    pub status: AccountStatus,
    pub email_verified: bool,
    pub suspension_reason: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub suspended_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub last_login_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub created_at: OffsetDateTime,
    /// Live login sessions. Tells an operator whether a suspension will actually take effect
    /// without also revoking sessions.
    pub active_sessions: i64,
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
    pub request: crate::me::PrivacyRequestView,
    /// The subject's id, or `None` once they have been erased.
    #[schema(value_type = Option<String>)]
    pub user_id: Option<Uuid>,
    /// The subject's username. `None` means the account is gone — for a completed erasure
    /// that is the expected end state, not missing data.
    pub username: Option<String>,
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
    /// One id per run planned — one per provider when the request names none.
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
    pub id: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub decided_at: OffsetDateTime,
    /// Groups every decision of one sweep run; absent for an operator's console merge.
    pub sweep_id: Option<Uuid>,
    /// `sweep_new` | `sweep_requeue` | `sweep_recheck` | `operator`.
    pub trigger: String,
    pub actor: Option<Uuid>,
    pub left_id: SeriesId,
    pub right_id: SeriesId,
    pub left_title: String,
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
    pub survivor_id: Option<SeriesId>,
    pub absorbed_id: Option<SeriesId>,
    pub score: f32,
    /// The similarity the score started from, before any bonus or penalty.
    pub base_score: f32,
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
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub reverted_at: Option<OffsetDateTime>,
    pub reverted_by: Option<Uuid>,
    pub revert_reason: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub flagged_at: Option<OffsetDateTime>,
    pub flagged_by: Option<Uuid>,
    pub flag_reason: Option<String>,
}

/// What undoing an automatic merge put back.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = MergeReverted)]
pub struct MergeRevertedView {
    pub decision_id: Uuid,
    /// The series that exists again, under its original id.
    pub restored_id: SeriesId,
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
    pub id: Uuid,
    /// Groups one account reconciliation.
    pub run_id: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub decided_at: OffsetDateTime,
    pub user_id: Uuid,
    pub username: Option<String>,
    pub series_id: Option<SeriesId>,
    pub series_title: Option<String>,
    pub provider: String,
    pub external_id: Option<String>,
    /// `match` | `progress` | `status` | `series` | `metadata`.
    pub scope: String,
    /// `matched` | `unmatched` | `pull` | `push` | `create_remote` | `conflict` | `noop`
    /// | `skipped` | `import_status` | `enriched` | `unmapped`.
    pub action: String,
    /// The stable slug for why: `only_remote_changed`, `both_sides_changed_policy_remote_wins`,
    /// `title_match_above_threshold`, `blocked_by_operator`, …
    pub reason: String,
    pub policy: Option<String>,
    /// Whether anything was actually written. A run is mostly considerations.
    pub applied: bool,
    pub local_before: Option<String>,
    pub local_after: Option<String>,
    pub remote_before: Option<String>,
    pub remote_after: Option<String>,
    /// The three-way merge's common ancestor. Without it a pull cannot be told from a clobber.
    pub ancestor_local: Option<String>,
    pub ancestor_remote: Option<String>,
    pub match_score: Option<f32>,
    pub match_signals: Vec<String>,
    /// Which titles matched, the scored terms, the runner-up, and the provider's own metadata.
    pub evidence: Json,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub reverted_at: Option<OffsetDateTime>,
    pub reverted_by: Option<Uuid>,
    pub revert_reason: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub flagged_at: Option<OffsetDateTime>,
    pub flagged_by: Option<Uuid>,
    pub flag_reason: Option<String>,
}

/// What undoing a sync decision put back.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = SyncReverted)]
pub struct SyncRevertedView {
    pub decision_id: Uuid,
    /// `local_progress` | `local_status` | `watchlist_entry` | `remote_entry` | `match`.
    pub restored: String,
    /// The value the restored side now holds.
    pub value: Option<String>,
    /// Whether the revert also refused the title match permanently.
    pub blocked_match: bool,
}
