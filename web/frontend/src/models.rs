//! Frontend DTOs.
//!
//! Every request/response shape that has a concrete Rust type on the backend is now
//! **generated** at compile time from the api service's `utoipa` schema (`crate::wire`, via
//! `xtask openapi` + `typify` — see that module's doc comment). This module re-exports those
//! generated types — renaming a handful to the names views already used, since a couple of
//! them (e.g. `AdminSyncAccount`/`AdminAccountRow`) drifted from the backend's naming when
//! this frontend model was still hand-written — and adds the presentation-only helpers
//! (`label()`, `token()`, `parse()`, `ALL`/`COLUMNS`, `progress()`) that don't belong in a
//! generated type. Types that aren't 1:1 JSON bodies (a query-string filter, a paged-response
//! wrapper) or that the backend itself only proxies as opaque JSON (external-sync status,
//! in-app notifications) stay hand-written below.

use serde::{Deserialize, Serialize};

pub use crate::wire::{
    ChapterDto, ContentType, ContinueItem, FeedEntry, LoginRequest, MeStats, ProfileDto,
    ProgressUpdate, Provider, ProviderId, ProviderStat, PublicProvider, RegisterRequest, RunState,
    ScanMode, ScanRun, SeriesDetail, SeriesId, SeriesSourceId, SeriesStatus, SeriesSummary,
    SessionDto, SourceDto, SuggestedMatch, SystemStats, Tag, TokenResponse, UserId, WatchStatus,
    WatchlistItem, WatchlistUpsert,
};
// Renamed re-exports: these frontend names predate the shared schema and are kept so the
// views that already use them don't need to change.
pub use crate::wire::AdminAccountRow as AdminSyncAccount;
pub use crate::wire::AdminMappingRow as AdminSyncMapping;
pub use crate::wire::AuditView as AuditEntry;
pub use crate::wire::FailedTaskView as FailedTask;
pub use crate::wire::MergeCandidateView as MergeCandidate;
pub use crate::wire::RemoteEntryRow as UnmatchedRemoteEntry;
pub use crate::wire::UnmappedSeriesRow as UnmappedSeries;
pub use crate::wire::UserRow2 as UserRow;

impl ContentType {
    pub fn label(self) -> &'static str {
        match self {
            Self::Manga => "Manga",
            Self::Manhwa => "Manhwa",
            Self::Manhua => "Manhua",
            Self::Webtoon => "Webtoon",
            Self::Unknown => "Unknown",
        }
    }
    pub const ALL: [ContentType; 5] = [
        Self::Manga,
        Self::Manhwa,
        Self::Manhua,
        Self::Webtoon,
        Self::Unknown,
    ];
    /// The lowercase token the API expects in the `content_type` query param.
    pub fn token(self) -> &'static str {
        match self {
            Self::Manga => "manga",
            Self::Manhwa => "manhwa",
            Self::Manhua => "manhua",
            Self::Webtoon => "webtoon",
            Self::Unknown => "unknown",
        }
    }
}

impl SeriesStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Ongoing => "Ongoing",
            Self::Completed => "Completed",
            Self::Hiatus => "Hiatus",
            Self::Cancelled => "Cancelled",
            Self::Unknown => "Unknown",
        }
    }
    pub const ALL: [SeriesStatus; 5] = [
        Self::Ongoing,
        Self::Completed,
        Self::Hiatus,
        Self::Cancelled,
        Self::Unknown,
    ];
    /// The lowercase token the API expects in the `status` query param.
    pub fn token(self) -> &'static str {
        match self {
            Self::Ongoing => "ongoing",
            Self::Completed => "completed",
            Self::Hiatus => "hiatus",
            Self::Cancelled => "cancelled",
            Self::Unknown => "unknown",
        }
    }
}

impl WatchStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Reading => "Reading",
            Self::Planned => "Planned",
            Self::Completed => "Completed",
            Self::Dropped => "Dropped",
            Self::Paused => "Paused",
        }
    }
    /// The columns shown on the Watchlist board (design §17.2.4), in order.
    pub const COLUMNS: [WatchStatus; 5] = [
        Self::Reading,
        Self::Planned,
        Self::Completed,
        Self::Paused,
        Self::Dropped,
    ];
}

impl RunState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Queued => "Queued",
            Self::Running => "Running",
            Self::Completed => "Completed",
            Self::Failed => "Failed",
            Self::Cancelled => "Cancelled",
        }
    }
}

impl ScanRun {
    /// Completion fraction in `0.0..=1.0` for the progress bar.
    pub fn progress(&self) -> f64 {
        if self.total_tasks <= 0 {
            return 0.0;
        }
        f64::from(self.done_tasks + self.failed_tasks) / f64::from(self.total_tasks)
    }
}

/// The reconciliation policy for `AniList` pull/push when a series exists on both sides
/// (Sync & integrations panel; mirrors `services/sync`'s `mapping::ConflictPolicy`). The
/// backend represents this as a plain `String` token (it's forwarded opaquely through
/// `services/api`'s sync proxy, never typed there either), so this stays hand-written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(clippy::enum_variant_names)]
pub enum ConflictPolicy {
    LocalWins,
    RemoteWins,
    NewestWins,
    AskMe,
}

impl ConflictPolicy {
    /// Plain-language label for the account Sync panel (design v2 §B.8).
    pub fn label(self) -> &'static str {
        match self {
            Self::LocalWins => "Always keep my local progress",
            Self::RemoteWins => "Always trust AniList",
            Self::NewestWins => "Use whichever changed most recently",
            Self::AskMe => "Ask me when they disagree",
        }
    }
    /// The token the API expects in a sync request body.
    pub fn token(self) -> &'static str {
        match self {
            Self::LocalWins => "local_wins",
            Self::RemoteWins => "remote_wins",
            Self::NewestWins => "newest_wins",
            Self::AskMe => "ask_me",
        }
    }
    /// Parse a policy token from the API, falling back to `NewestWins` (the zero-config
    /// default, design v2 §B.8).
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

// ----- opaque-proxy shapes (services/api forwards these as untyped JSON; see
// services/api/src/openapi.rs's doc comment for why they aren't generated) -----

/// `AniList` link status (`GET /v1/me/sync/anilist/status`): whether the caller has a linked
/// account, its display name, and the most recent sync time. Always present — `linked: false`
/// means unlinked rather than a missing resource.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct SyncStatus {
    pub linked: bool,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub last_synced_at: Option<String>,
}

/// A registered external sync provider (`GET /v1/me/sync/providers`). Drives the Account
/// "Sync & integrations" panel, which renders one card per entry instead of a hardcoded
/// AniList block (design: generalized multi-provider sync).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ProviderInfo {
    pub slug: String,
    pub name: String,
}

/// A provider's automatic-sync settings for the account panel (design v2 §B.6/§B.8).
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct SyncSettings {
    pub linked: bool,
    #[serde(default)]
    pub auto_sync_enabled: bool,
    #[serde(default)]
    pub conflict_policy: String,
    #[serde(default)]
    pub pending_conflicts: i64,
}

/// One pending sync conflict awaiting the user's decision (design v2 §B.6).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SyncConflict {
    pub id: String,
    pub series_id: String,
    pub series_title: String,
    pub provider: String,
    pub field: String,
    pub local_value: String,
    pub remote_value: String,
    pub detected_at: String,
}

/// One entry of the user-facing sync history (design v2 §B.6).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SyncHistoryEntry {
    pub id: String,
    pub series_id: String,
    pub series_title: String,
    pub provider: String,
    pub action: String,
    #[serde(default)]
    pub detail: serde_json::Value,
    pub created_at: String,
}

/// In-app notification row (`/v1/me/notifications`; payload shape is open JSON).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Notification {
    pub id: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub payload: serde_json::Value,
    #[serde(default)]
    pub read_at: Option<String>,
    #[serde(default)]
    pub created_at: String,
}

/// Live push from `/v1/me/stream` (mirrors `tankovault_contracts::UserNotification`). Only the
/// fields the badge and toast need are decoded; the rest are ignored by serde.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct LiveNotification {
    /// The recipient's unread count including this notification — set the rail badge to it.
    pub unread_count: i64,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub payload: serde_json::Value,
}

// ----- frontend-only client shapes (not 1:1 JSON bodies) -----

/// Server-side Discover filter (§9.1). Serialised to a `GET /v1/series` query string by
/// `api::list_series_filtered`; every field is optional so it degrades to a plain browse.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SeriesFilter {
    pub query: Option<String>,
    pub content_type: Option<ContentType>,
    pub status: Option<SeriesStatus>,
    pub provider: Option<String>,
    pub tags: Vec<String>,
    pub exclude_tags: Vec<String>,
    pub year_min: Option<i32>,
    pub year_max: Option<i32>,
    pub min_chapters: Option<i32>,
    pub sort: Option<String>,
    pub page: i64,
    pub limit: i64,
}

/// One page of Discover results (`GET /v1/series`, §9.1): the array body plus the
/// `X-Total-Count` / `X-Next-Cursor` header metadata for the pager.
#[derive(Debug, Clone, PartialEq)]
pub struct SeriesPage {
    pub items: Vec<SeriesSummary>,
    pub total: i64,
    pub next_cursor: Option<i64>,
}
