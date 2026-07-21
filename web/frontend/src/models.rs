//! Frontend DTOs. These mirror the API's JSON contract (services/api, design §11)
//! and the domain enum tokens (crates/domain/src/enums.rs — lowercase snake_case).
//!
//! Kept as an independent, `String`-id model rather than depending on the backend
//! `contracts`/`domain` crates so the WASM build stays lean and decoupled; the shapes
//! are validated against the API handlers.

use serde::{Deserialize, Serialize};

/// The medium/origin classification of a work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentType {
    Manga,
    Manhwa,
    Manhua,
    Webtoon,
    Unknown,
}

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
}

/// Publication status of a canonical series.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeriesStatus {
    Ongoing,
    Completed,
    Hiatus,
    Cancelled,
    Unknown,
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
}

/// A user's tracking status for a series.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WatchStatus {
    Reading,
    Planned,
    Completed,
    Dropped,
    Paused,
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

/// Lifecycle of a scan run (operator console).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
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

/// Scan cadence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanMode {
    Full,
    Fast,
}

/// The reconciliation policy for `AniList` pull/push when a series exists on both sides
/// (Sync & integrations panel; mirrors `services/sync`'s `mapping::ConflictPolicy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(clippy::enum_variant_names)]
pub enum ConflictPolicy {
    LocalWins,
    RemoteWins,
    NewestWins,
}

impl ConflictPolicy {
    pub fn label(self) -> &'static str {
        match self {
            Self::LocalWins => "Local wins",
            Self::RemoteWins => "Remote wins",
            Self::NewestWins => "Newest wins",
        }
    }
    /// The token the API expects in a sync request body.
    pub fn token(self) -> &'static str {
        match self {
            Self::LocalWins => "local_wins",
            Self::RemoteWins => "remote_wins",
            Self::NewestWins => "newest_wins",
        }
    }
    pub const ALL: [ConflictPolicy; 3] = [Self::LocalWins, Self::RemoteWins, Self::NewestWins];
}

// ----- auth -----

#[derive(Debug, Clone, Serialize)]
pub struct RegisterRequest {
    pub email: String,
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct LoginRequest {
    /// Email or username.
    pub login: String,
    pub password: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    #[allow(dead_code)]
    pub token_type: String,
    #[allow(dead_code)]
    pub expires_in: i64,
}

// ----- series -----

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SeriesSummary {
    pub id: String,
    pub title: String,
    pub cover_url: Option<String>,
    pub content_type: ContentType,
    pub status: SeriesStatus,
    pub source_count: i64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SourceDto {
    pub id: String,
    pub provider_name: String,
    pub provider_slug: String,
    pub url: String,
    pub chapter_count: i32,
    /// True for the richest source (most chapters) — the reader should prefer it (§9.2).
    #[serde(default)]
    pub is_primary: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SeriesDetail {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub cover_url: Option<String>,
    pub content_type: ContentType,
    pub status: SeriesStatus,
    pub release_year: Option<i32>,
    pub sources: Vec<SourceDto>,
    /// Alternative titles gathered across providers (§9.2; empty when none).
    #[serde(default)]
    pub alt_titles: Vec<String>,
    /// Genre/tags attached to the series (§9.2; empty when none).
    #[serde(default)]
    pub tags: Vec<Tag>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ChapterDto {
    pub number: f64,
    pub title: Option<String>,
    pub url: String,
    pub published_at: Option<String>,
    /// Whether the requesting user has read this chapter (§9.2). `None` when anonymous.
    #[serde(default)]
    pub read: Option<bool>,
}

/// Genre/tag (Discover filter + Series tag chips — see `api::tags`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Tag {
    pub id: String,
    pub slug: String,
    pub name: String,
}

// ----- me -----

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct WatchlistItem {
    pub series_id: String,
    pub status: WatchStatus,
    pub notify: bool,
    pub added_at: String,
    /// Embedded series title so boards render without a per-card detail fetch (§9.3).
    #[serde(default)]
    pub series_title: String,
    #[serde(default)]
    pub cover_url: Option<String>,
    /// The user's last-read chapter number, if any.
    #[serde(default)]
    pub last_read_number: Option<f64>,
    /// Unread chapters above the user's progress.
    #[serde(default)]
    pub unread: i64,
}

/// A continue-reading card (`GET /v1/me/continue`, §9.3): tracked, in-progress series with
/// unread chapters, freshest activity first.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ContinueItem {
    pub series_id: String,
    pub series_title: String,
    #[serde(default)]
    pub cover_url: Option<String>,
    pub last_read_number: f64,
    #[serde(default)]
    pub next_number: Option<f64>,
    pub unread: i64,
}

/// Lifetime tracking stats for the Home / Profile headline (`GET /v1/me/stats`, §9.3).
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct MeStats {
    pub tracking: i64,
    pub reading: i64,
    pub completed: i64,
    pub chapters_read: i64,
    pub unread: i64,
}

/// The caller's identity (`PATCH /v1/me/profile`, §9.4).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ProfileDto {
    pub id: String,
    pub email: String,
    pub username: String,
    pub role: String,
}

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

/// One row of the admin Console's "Linked accounts" table (`GET /v1/admin/sync/accounts`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct AdminSyncAccount {
    pub user_id: String,
    pub username: String,
    pub provider: String,
    #[serde(default)]
    pub external_username: Option<String>,
    #[serde(default)]
    pub last_synced_at: Option<String>,
    #[serde(default)]
    pub last_error: Option<String>,
    pub created_at: String,
}

/// One row of the admin Console's "Series mappings" table (`GET /v1/admin/sync/mappings`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct AdminSyncMapping {
    pub series_id: String,
    pub series_title: String,
    pub provider: String,
    pub external_id: String,
    pub updated_at: String,
}

/// One row of the admin Console's Sync "Assign queue" (`GET /v1/admin/sync/unmapped`): a
/// series with no external mapping for the selected provider yet, awaiting a manual link.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct UnmappedSeries {
    pub series_id: String,
    pub series_title: String,
    #[serde(default)]
    pub source_count: i64,
}

/// One row of the admin Console's Sync "Match every loaded entry" queue
/// (`GET /v1/admin/sync/unmatched`): a fetched remote entry the auto-matcher could not
/// confidently link to a local series, awaiting a manual assignment.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct UnmatchedRemoteEntry {
    pub user_id: String,
    pub username: String,
    pub provider: String,
    pub external_id: String,
    pub title: String,
    pub status: String,
    #[serde(default)]
    pub progress: f64,
    #[serde(default)]
    pub content_type: String,
    #[serde(default)]
    pub start_year: Option<i32>,
}

/// A ranked suggestion for the admin "match every loaded entry" screen
/// (`GET /v1/admin/sync/suggest`): a local series the matcher thinks a fetched remote entry
/// could be, with a confidence `score` in `[0,1]` and enough info to eyeball and inspect it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SuggestedMatch {
    pub series_id: String,
    pub title: String,
    #[serde(default)]
    pub content_type: String,
    #[serde(default)]
    pub release_year: Option<i32>,
    #[serde(default)]
    pub source_count: i64,
    #[serde(default)]
    pub score: f32,
}

/// One active login session (`GET /v1/me/sessions`, §9.4).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SessionDto {
    pub id: String,
    pub family_id: String,
    pub created_at: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct WatchlistUpsert {
    pub status: WatchStatus,
    pub notify: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProgressUpdate {
    pub last_read_number: f64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct FeedEntry {
    pub series_id: String,
    pub series_title: String,
    pub chapter_number: f64,
    pub chapter_title: Option<String>,
    pub provider_slug: String,
    pub url: String,
    pub discovered_at: String,
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

// ----- admin / console -----

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ScanRun {
    pub id: String,
    pub provider_id: Option<String>,
    pub mode: ScanMode,
    pub state: RunState,
    pub total_tasks: i32,
    pub done_tasks: i32,
    pub failed_tasks: i32,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub finished_at: Option<String>,
    #[serde(default)]
    pub created_at: String,
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

/// Per-provider crawl politeness (mirrors `tankovault_domain::Politeness`). Editable in the
/// operator console; the server clamps every value to its hard ceilings on write.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Politeness {
    #[serde(default = "Politeness::default_rps")]
    pub rps: f64,
    #[serde(default = "Politeness::default_concurrency")]
    pub concurrency: u32,
    #[serde(default)]
    pub crawl_delay_ms: u64,
    #[serde(default)]
    pub user_agent: String,
}

impl Politeness {
    fn default_rps() -> f64 {
        1.0
    }
    fn default_concurrency() -> u32 {
        2
    }
}

impl Default for Politeness {
    fn default() -> Self {
        Self {
            rps: Self::default_rps(),
            concurrency: Self::default_concurrency(),
            crawl_delay_ms: 0,
            user_agent: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Provider {
    pub id: String,
    pub slug: String,
    pub name: String,
    pub base_url: String,
    #[serde(default)]
    pub state: String,
    /// Adapter implementation token (`madara` | `generic_config` | `custom`); immutable.
    #[serde(default)]
    pub adapter: String,
    /// Adapter selector/pagination config; shape depends on `adapter`.
    #[serde(default)]
    pub config: serde_json::Value,
    #[serde(default)]
    pub politeness: Politeness,
}

/// System-wide rollup for the console header (`GET /v1/admin/stats`).
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct SystemStats {
    pub providers_total: i64,
    pub providers_active: i64,
    pub providers_disabled: i64,
    pub providers_unhealthy: i64,
    pub series_total: i64,
    pub sources_total: i64,
    pub chapters_total: i64,
    pub chapters_1h: i64,
    pub chapters_24h: i64,
    pub chapters_7d: i64,
    pub users_total: i64,
    pub pending_merges: i64,
    pub runs_active: i64,
    pub runs_running: i64,
    pub tasks_queued: i64,
    pub tasks_running: i64,
    pub tasks_failed_24h: i64,
}

/// One row of the per-provider statistics table (`GET /v1/admin/providers/stats`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ProviderStat {
    pub provider_id: String,
    pub slug: String,
    pub name: String,
    pub state: String,
    pub adapter: String,
    pub series_count: i64,
    pub source_count: i64,
    pub blocked_sources: i64,
    pub chapter_count: i64,
    pub chapters_24h: i64,
    pub chapters_7d: i64,
    #[serde(default)]
    pub last_chapter_at: Option<String>,
    #[serde(default)]
    pub last_scanned_at: Option<String>,
    #[serde(default)]
    pub last_full_scan_at: Option<String>,
    #[serde(default)]
    pub last_run_state: Option<String>,
    #[serde(default)]
    pub last_run_at: Option<String>,
}

/// A recently-failed scan task with its error (`GET /v1/admin/scan-failures`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct FailedTask {
    pub id: String,
    pub run_id: String,
    #[serde(default)]
    pub provider_slug: Option<String>,
    pub mode: ScanMode,
    pub kind: String,
    #[serde(default)]
    pub error: Option<String>,
    pub attempts: i16,
    #[serde(default)]
    pub finished_at: Option<String>,
}

/// A recent privileged action from the audit trail (`GET /v1/admin/audit`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct AuditEntry {
    pub id: String,
    #[serde(default)]
    pub actor: Option<String>,
    pub action: String,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub detail: serde_json::Value,
    #[serde(default)]
    pub created_at: String,
}

/// A pending canonicalisation merge candidate (`GET /v1/admin/merge-candidates`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct MergeCandidate {
    pub id: String,
    pub series_id: String,
    pub series_title: String,
    pub candidate_id: String,
    pub candidate_title: String,
    pub score: f64,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub created_at: String,
}

/// A public-facing provider entry for the Discover filter (`GET /v1/providers`, §9.3):
/// identity + how many distinct series it carries.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PublicProvider {
    pub id: String,
    pub slug: String,
    pub name: String,
    #[serde(default)]
    pub series_count: i64,
}

/// One row of the operator Users directory (`GET /v1/admin/users`, §9.5).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct UserRow {
    pub id: String,
    pub email: String,
    pub username: String,
    /// RBAC role token (`user` | `operator` | `admin`).
    pub role: String,
    #[serde(default)]
    pub tracked_count: i64,
    #[serde(default)]
    pub created_at: String,
}

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

impl ContentType {
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
