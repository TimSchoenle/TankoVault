//! The console header's rollup, per-provider statistics, and the audit log.

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
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
    /// Providers in a non-serving health state: degraded, challenged, solving or blocked.
    pub providers_unhealthy: i64,
    /// Canonical series, not provider pages.
    pub series_total: i64,
    /// Series-to-provider links across every provider.
    pub sources_total: i64,
    /// Chapter links held, part releases counted separately.
    pub chapters_total: i64,
    /// Chapters first seen in the last hour, by discovery time, not publication date.
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
    /// The subset of `runs_active` that has started.
    pub runs_running: i64,
    /// Tasks waiting for a worker across every run.
    pub tasks_queued: i64,
    /// Tasks claimed or fetching.
    pub tasks_running: i64,
    /// Tasks that failed in the last 24 hours, by the time they settled.
    pub tasks_failed_24h: i64,
}

/// One row of the per-provider statistics table.
///
/// Enum columns are cast to text, and the provider's identity fields are joined in so the console
/// renders the table from one fetch.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = ProviderStat)]
pub struct ProviderStatView {
    /// The provider these figures are for.
    pub provider_id: Uuid,
    /// Its slug, joined in so the table renders from one fetch.
    pub slug: String,
    /// Its display name.
    pub name: String,
    /// Provider health state token, such as `active`, `disabled` or `blocked`.
    pub state: String,
    /// Adapter implementation token: `madara`, `generic_config` or `custom`.
    pub adapter: String,
    /// Distinct series that have at least one source on this provider.
    pub series_count: i64,
    /// Source links, joining a series to this provider, that this provider owns.
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
    /// Actor username, `null` for a system action or a deleted user.
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

/// A page of the audit trail plus how many records the filter matches in total.
///
/// An envelope, not a bare list: the console only ever holds a window on the trail, and a pager
/// with no total cannot say where it is.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AuditPageView {
    /// The page, newest record first.
    pub items: Vec<AuditView>,
    /// Total matching the current filter, ignoring `limit` and `offset`.
    pub total: i64,
}
