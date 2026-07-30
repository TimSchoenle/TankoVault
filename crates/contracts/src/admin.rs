//! Operator-console response bodies.
//!
//! These are the shapes `services/api` publishes on `/v1/admin/*`. They live here rather than
//! in `tankovault-db` because a repository row is a *query result*, not a promise to a client:
//! when the admin console read models carried `ToSchema` directly on the row struct, renaming
//! a column in the `SELECT` silently rewrote the public API and the generated client, with no
//! compile error anywhere — the handler never named a field, so nothing connected the two.
//!
//! The conversion from the row lives in `services/api` (the only crate allowed to know both
//! layers), which is what turns that class of change back into a compile error at exactly one
//! call site.
//!
//! Every type pins its `OpenAPI` component name with `#[schema(as = ...)]` where the Rust name
//! differs from the published one. The move is an internal layering fix; it must not rename
//! anything on the wire, because `crates/api-client` and the frontend are generated from these
//! names.

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use tankovault_domain::{AccountStatus, ScanRunId, SeriesId};
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

/// A pending merge candidate enriched with both series' display titles, for the operator
/// review queue (design §11 `GET /v1/admin/merge-candidates`).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MergeCandidateView {
    pub id: Uuid,
    pub series_id: SeriesId,
    pub series_title: String,
    pub candidate_id: SeriesId,
    pub candidate_title: String,
    pub score: f32,
    pub reason: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub created_at: OffsetDateTime,
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
    pub permission_count: i64,
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
