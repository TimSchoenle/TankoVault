//! Pure domain entities. These mirror the persistent rows but carry no persistence
//! concerns — the `db` crate maps SQL rows onto them. Chapter numbers are `f64` here
//! to match the adapter contract; the DB stores them as `numeric(10,4)`.

use crate::enums::{
    AccountStatus, AdapterKind, ContentType, ProviderState, RunState, ScanMode, SeriesStatus,
    TaskState, WatchStatus,
};
use crate::ids::{
    AuthorId, NotificationId, ProviderId, ScanRunId, ScanTaskId, SeriesId, SeriesSourceId, TagId,
    UserId,
};
use crate::politeness::Politeness;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use utoipa::ToSchema;

/// A source site. The single place a domain (`base_url`) is defined.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Provider {
    pub id: ProviderId,
    pub slug: String,
    pub name: String,
    /// The domain root. Changing this one field migrates every stored link.
    pub base_url: String,
    pub adapter: AdapterKind,
    /// Adapter selector/pagination config (shape depends on `adapter`).
    pub config: serde_json::Value,
    pub state: ProviderState,
    pub politeness: Politeness,
    /// How this row relates to the built-in preset catalogue; `None` for a provider an
    /// operator registered by hand.
    pub preset: Option<PresetLink>,
    /// Wire shape is a plain string (whatever `time`'s serde impl emits), never parsed
    /// client-side — kept untyped here so the generated frontend type doesn't need a date
    /// crate (mirrors the `no date crate in the bundle` constraint on the other timestamp
    /// fields below).
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub last_full_scan_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub updated_at: OffsetDateTime,
}

/// A provider row's link to the preset catalogue shipped with the build.
///
/// `locked` is the contract the whole feature turns on: while it holds, the installer
/// overwrites the **preset-owned fields** — `name`, `base_url`, `adapter` and `config` — from
/// the shipped definition on every rollout, so a selector fix reaches deployments that already
/// carry the row. Unlocking detaches the row for good; it keeps the slug so the console can
/// still offer a re-link, but nothing rewrites it again unless an operator asks.
///
/// `politeness` and `state` sit outside the lock on purpose, and must stay there: a crawl
/// budget and a pause are an operator's answer to their own infrastructure, robots policy and
/// legal position, none of which a shipped preset can know. A lock that silently restored a
/// rate limit an operator had lowered would be a worse bug than a stale selector.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PresetLink {
    /// The governing preset — the row's own slug for anything the installer created.
    pub slug: String,
    /// Whether the preset-owned fields still follow the shipped definition.
    pub locked: bool,
    /// When those fields were last written from the preset.
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub synced_at: Option<OffsetDateTime>,
}

/// One entry of the preset catalogue bundled with the build, as the installer recorded it.
///
/// Persisted rather than read from `tankovault_adapters`, because the api tier deliberately
/// does not link the adapter crate (it would drag `BoringSSL` into that image); the install job
/// writes the catalogue down and every other tier reads it as data.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PresetDefinition {
    pub slug: String,
    pub name: String,
    pub base_url: String,
    pub adapter: AdapterKind,
    pub config: serde_json::Value,
    /// What a *new* provider from this preset starts at. Never re-applied to an existing row —
    /// see [`PresetLink`] on why politeness is outside the lock.
    pub politeness: Politeness,
    /// When the installer last recorded this entry, which is also the answer to "did my
    /// rollout run the install job at all?".
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub updated_at: OffsetDateTime,
}

/// The canonical, provider-independent work.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Series {
    pub id: SeriesId,
    pub canonical_title: String,
    pub normalized_title: String,
    pub description: Option<String>,
    pub cover_url: Option<String>,
    pub content_type: ContentType,
    pub status: SeriesStatus,
    pub release_year: Option<i32>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// An alternative title that aids cross-provider matching.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeriesTitle {
    pub series_id: SeriesId,
    pub title: String,
    pub normalized: String,
}

/// A genre/tag.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Tag {
    pub id: TagId,
    pub slug: String,
    pub name: String,
}

/// An author/artist credit.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Author {
    pub id: AuthorId,
    pub slug: String,
    pub name: String,
}

/// The join: one canonical series existing at one provider under one path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeriesSource {
    pub id: SeriesSourceId,
    pub series_id: SeriesId,
    pub provider_id: ProviderId,
    /// RELATIVE path; resolve against `provider.base_url` at read time.
    pub source_path: String,
    pub provider_title: Option<String>,
    /// Hash of last-seen metadata + chapter list, for cheap change detection.
    pub content_hash: Option<Vec<u8>>,
    pub chapter_count: i32,
    pub last_scanned_at: Option<OffsetDateTime>,
    pub state: ProviderState,
}

/// A chapter link under a [`SeriesSource`]. Never image data.
///
/// Carries no id: `(series_source_id, number)` identifies a chapter, and migration 0055 made that
/// the primary key. `path` is the **expanded** site-relative link — the repo layer undoes the
/// prefix compression `chapters.path` is stored with, so nothing above it has to know about it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chapter {
    pub series_source_id: SeriesSourceId,
    pub number: f64,
    pub title: Option<String>,
    /// RELATIVE link to the chapter page.
    pub path: String,
    pub published_at: Option<OffsetDateTime>,
    pub discovered_at: OffsetDateTime,
}

/// A user account. The password hash lives only in the `db`/`auth` layers, never here.
///
/// Carries no authorization state — permissions are resolved per request from a separate
/// grant store, so an identity record cannot go stale against a revoked grant.
/// [`AccountStatus`] lives here because it is identity, not authorization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: UserId,
    pub email: String,
    pub username: String,
    pub status: AccountStatus,
    pub created_at: OffsetDateTime,
}

/// A watchlist membership with per-title notification opt-in.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchlistEntry {
    pub user_id: UserId,
    pub series_id: SeriesId,
    pub status: WatchStatus,
    pub notify: bool,
    pub added_at: OffsetDateTime,
}

/// A user's read position within a series.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadProgress {
    pub user_id: UserId,
    pub series_id: SeriesId,
    pub last_read_number: f64,
    pub updated_at: OffsetDateTime,
}

/// An in-app notification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    pub id: NotificationId,
    pub user_id: UserId,
    pub kind: String,
    pub payload: serde_json::Value,
    #[serde(with = "time::serde::rfc3339::option")]
    pub read_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// A scan run (progress + audit; mirrors `JetStream` dispatch).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ScanRun {
    pub id: ScanRunId,
    pub provider_id: Option<ProviderId>,
    pub mode: ScanMode,
    pub state: RunState,
    pub total_tasks: i32,
    pub done_tasks: i32,
    pub failed_tasks: i32,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub started_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub finished_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub created_at: OffsetDateTime,
}

/// A single unit of scan work.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanTask {
    pub id: ScanTaskId,
    pub run_id: ScanRunId,
    /// `catalog_page` | `series` | `latest_feed`.
    pub kind: String,
    /// Task target, e.g. `{"path":"/manga/x","page":3}`.
    pub target: serde_json::Value,
    pub state: TaskState,
    pub attempts: i16,
    pub worker_id: Option<String>,
    pub error: Option<String>,
    pub claimed_at: Option<OffsetDateTime>,
    pub finished_at: Option<OffsetDateTime>,
}

impl Provider {
    /// Resolve a relative path stored under this provider into an absolute URL.
    ///
    /// # Errors
    /// Propagates [`crate::link::ResolveError`] from the resolver.
    pub fn resolve(&self, path: &str) -> Result<String, crate::link::ResolveError> {
        crate::link::resolve_link(&self.base_url, path)
    }
}
