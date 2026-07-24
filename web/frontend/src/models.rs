//! Frontend DTOs.
//!
//! Every request/response shape that has a concrete Rust type on the backend is now
//! **generated** at compile time from the api service's `utoipa` schema (`crate::wire`, via
//! `xtask openapi` + `progenitor`). This module re-exports those
//! generated types — renaming a handful to the names views already used, since a couple of
//! them (e.g. `AdminSyncAccount`/`AdminAccountRow`) drifted from the backend's naming when
//! this frontend model was still hand-written — and adds the presentation-only helpers
//! that don't belong in a generated type.

use serde::{Deserialize, Serialize};

// Typed DTOs from the types module (to avoid name collisions with builders)
pub use crate::wire::types::{
    AdapterKind, AssignRemoteEntry, ChapterDto, ChapterRead, ContentType, ContinueItem,
    CreateProvider, DismissRequest, FeedEntry, LoginRequest, MarkRead, MergeRequest, Politeness,
    ProfileUpdate, ProgressUpdate, Provider, ProviderId, ProviderStat, ProviderState,
    PublicProvider, RegisterRequest, ResolveConflict, RunState, ScanMode, ScanRun,
    ScanRunProviderId, SeriesId, SeriesSourceId, SeriesStatus, SeriesSummary,
    SetProviderState as SetProviderStateBody, SourceDto, SuggestedMatch, SyncExcluded, SyncOpts,
    SyncPullBody, SyncPushBody, SyncSettingsPatch, SystemStats, Tag, TriggerScan,
    TriggerScanProviderId, UpdateProvider, UpsertMapping, UserId, UserRow2 as UserRow, WatchStatus,
    WatchlistItem, WatchlistUpsert,
};

pub type Notification = serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveNotification {
    pub unread_count: i64,
}

// Renamed re-exports from types
pub use crate::wire::types::AdminAccountRow as AdminSyncAccount;
pub use crate::wire::types::AdminMappingRow as AdminSyncMapping;
pub use crate::wire::types::AuditView as AuditEntry;
pub use crate::wire::types::FailedTaskView as FailedTask;
pub use crate::wire::types::MergeCandidateView as MergeCandidate;
pub use crate::wire::types::RemoteEntryRow as UnmatchedRemoteEntry;
pub use crate::wire::types::UnmappedSeriesRow as UnmappedSeries;

// Forwarded/Untyped types (manual definitions as they are forwarded verbatim)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncStatus {
    pub linked: bool,
    pub display_name: Option<String>,
    /// RFC3339 timestamp string from the sync service (no date crate in the wasm bundle).
    pub last_sync: Option<String>,
}

/// Pagination metadata for `GET /v1/series` (body is a plain `Vec<SeriesSummary>`; total and
/// next-page ride on response headers).
#[derive(Debug, Clone)]
pub struct SeriesPage {
    pub items: Vec<SeriesSummary>,
    pub total: i64,
    pub next_cursor: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderInfo {
    #[serde(default)]
    pub id: String,
    pub name: String,
    /// Provider slug used in sync paths and mapping keys.
    #[serde(default)]
    pub slug: String,
    #[serde(default)]
    pub linked: bool,
}

impl ProviderInfo {
    /// Prefer the explicit slug; fall back to `id` when the payload only carries one key.
    pub fn slug_or_id(&self) -> &str {
        if self.slug.is_empty() {
            &self.id
        } else {
            &self.slug
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncSettings {
    pub auto_pull: bool,
    pub auto_push: bool,
    pub conflict_policy: String,
    pub auto_sync_enabled: bool,
    pub pending_conflicts: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SyncConflict {
    pub id: String,
    pub provider: String,
    pub series_id: SeriesId,
    pub series_title: String,
    pub field: String,
    pub local_value: String,
    pub remote_value: String,
}

// Re-export domain logic for types that are mapped to domain types
// (We define extension traits locally to avoid missing trait errors)

pub trait ContentTypeExt {
    fn label(&self) -> &'static str;
    fn token(&self) -> &'static str;
    fn all() -> &'static [ContentType];
}
impl ContentTypeExt for ContentType {
    fn label(&self) -> &'static str {
        match self {
            Self::Manga => "Manga",
            Self::Manhwa => "Manhwa",
            Self::Manhua => "Manhua",
            Self::Webtoon => "Webtoon",
            _ => "Unknown",
        }
    }
    fn token(&self) -> &'static str {
        match self {
            Self::Manga => "manga",
            Self::Manhwa => "manhwa",
            Self::Manhua => "manhua",
            Self::Webtoon => "webtoon",
            _ => "unknown",
        }
    }
    fn all() -> &'static [ContentType] {
        &[
            ContentType::Manga,
            ContentType::Manhwa,
            ContentType::Manhua,
            ContentType::Webtoon,
        ]
    }
}

pub trait SeriesStatusExt {
    fn label(&self) -> &'static str;
    fn token(&self) -> &'static str;
    fn all() -> &'static [SeriesStatus];
}
impl SeriesStatusExt for SeriesStatus {
    fn label(&self) -> &'static str {
        match self {
            Self::Ongoing => "Ongoing",
            Self::Completed => "Completed",
            Self::Hiatus => "Hiatus",
            Self::Cancelled => "Cancelled",
            _ => "Unknown",
        }
    }
    fn token(&self) -> &'static str {
        match self {
            Self::Ongoing => "ongoing",
            Self::Completed => "completed",
            Self::Hiatus => "hiatus",
            Self::Cancelled => "cancelled",
            _ => "unknown",
        }
    }
    fn all() -> &'static [SeriesStatus] {
        &[
            SeriesStatus::Ongoing,
            SeriesStatus::Completed,
            SeriesStatus::Hiatus,
            SeriesStatus::Cancelled,
        ]
    }
}

pub trait WatchStatusExt {
    fn label(&self) -> &'static str;
    fn columns() -> &'static [WatchStatus];
}
impl WatchStatusExt for WatchStatus {
    fn label(&self) -> &'static str {
        match self {
            Self::Reading => "Reading",
            Self::Planned => "Plan to read",
            Self::Completed => "Completed",
            Self::Dropped => "Dropped",
            Self::Paused => "On hold",
        }
    }
    fn columns() -> &'static [WatchStatus] {
        &[
            WatchStatus::Reading,
            WatchStatus::Planned,
            WatchStatus::Completed,
            WatchStatus::Paused,
            WatchStatus::Dropped,
        ]
    }
}

pub trait RunStateExt {
    fn label(&self) -> &'static str;
}
impl RunStateExt for RunState {
    fn label(&self) -> &'static str {
        match self {
            Self::Queued => "Queued",
            Self::Running => "Running",
            Self::Completed => "Finished",
            Self::Failed => "Failed",
            Self::Cancelled => "Cancelled",
        }
    }
}

pub trait ScanRunExt {
    fn progress(&self) -> f64;
}
impl ScanRunExt for ScanRun {
    fn progress(&self) -> f64 {
        if self.total_tasks <= 0 {
            return 0.0;
        }
        f64::from(self.done_tasks + self.failed_tasks) / f64::from(self.total_tasks)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictPolicy {
    LocalWins,
    RemoteWins,
    NewestWins,
    AskMe,
}

impl ConflictPolicy {
    pub fn label(self) -> &'static str {
        match self {
            Self::LocalWins => "Always keep my local progress",
            Self::RemoteWins => "Always trust AniList",
            Self::NewestWins => "Use whichever changed most recently",
            Self::AskMe => "Ask me when they disagree",
        }
    }
    pub fn token(self) -> &'static str {
        match self {
            Self::LocalWins => "local_wins",
            Self::RemoteWins => "remote_wins",
            Self::NewestWins => "newest_wins",
            Self::AskMe => "ask_me",
        }
    }
    pub fn parse(token: &str) -> Self {
        match token {
            "local_wins" => Self::LocalWins,
            "remote_wins" => Self::RemoteWins,
            "ask_me" => Self::AskMe,
            _ => Self::NewestWins,
        }
    }
    pub const ALL: [ConflictPolicy; 4] = [
        Self::LocalWins,
        Self::RemoteWins,
        Self::NewestWins,
        Self::AskMe,
    ];
}
