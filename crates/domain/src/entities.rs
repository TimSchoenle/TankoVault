//! Pure domain entities. These mirror the persistent rows but carry no persistence
//! concerns — the `db` crate maps SQL rows onto them. Chapter numbers are `f64` here
//! to match the adapter contract; the DB stores them as `numeric(10,4)`.

use crate::enums::{
    AdapterKind, ContentType, ProviderState, RunState, ScanMode, SeriesStatus, TaskState, UserRole,
    WatchStatus,
};
use crate::ids::{
    AuthorId, ChapterId, NotificationId, ProviderId, ScanRunId, ScanTaskId, SeriesId,
    SeriesSourceId, TagId, UserId,
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
    pub robots_txt: Option<String>,
    /// Wire shape is a plain string (whatever `time`'s serde impl emits), never parsed
    /// client-side — kept untyped here so the generated frontend type doesn't need a date
    /// crate (mirrors the `no date crate in the bundle` constraint on the other timestamp
    /// fields below).
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub robots_at: Option<OffsetDateTime>,
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chapter {
    pub id: ChapterId,
    pub series_source_id: SeriesSourceId,
    pub number: f64,
    pub volume: Option<i32>,
    pub title: Option<String>,
    /// RELATIVE link to the chapter page.
    pub path: String,
    pub published_at: Option<OffsetDateTime>,
    pub discovered_at: OffsetDateTime,
}

/// A user account. The password hash lives only in the `db`/`auth` layers, never here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: UserId,
    pub email: String,
    pub username: String,
    pub role: UserRole,
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
