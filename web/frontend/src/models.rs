//! Wire DTOs and the presentation-only helpers hung off them.
//!
//! **Every** request and response shape is generated at compile time from the API service's
//! `utoipa` schema (`xtask openapi` → `progenitor` → `tankovault-api-client`). This module
//! re-exports those types under the names the views use and adds the labelling/ordering
//! helpers that have no business living in generated code.
//!
//! Nothing here hand-mirrors a payload. It used to, for the `/v1/me/sync/*` endpoints the API
//! service proxies verbatim, and those mirrors drifted silently: the settings panel discarded
//! every persisted value because two fields it required no longer existed, and the connected
//! display name and last-sync time had been renamed out from under it. Those shapes now live
//! in `tankovault_contracts::sync`, are returned by the producing service, are declared on the
//! API's own routes, and arrive here generated — so that class of drift cannot recur.

use serde::{Deserialize, Serialize};

pub(crate) use crate::wire::types::{
    AccountStatus, AdapterKind, AssignRemoteEntry, ChapterDto, ChapterRead, ConflictRow,
    ContentType, ContinueItem, CreateProvider, DismissRequest, FeedEntry, ForgotPasswordRequest,
    LoginRequest, MarkRead, MergeRequest, Politeness, ProfileUpdate, ProgressUpdate, Provider,
    ProviderId, ProviderInfo, ProviderStat, ProviderState, PublicProvider, RegisterRequest,
    ResendVerificationRequest, ResetPasswordRequest, ResolveConflict, RunState, ScanMode, ScanRun,
    ScanRunProviderId, SeriesId, SeriesSourceId, SeriesStatus, SeriesSummary,
    SetProviderState as SetProviderStateBody, SourceDto, SuggestedMatch, SyncExcluded, SyncOpts,
    SyncPullBody, SyncPushBody, SyncSettingsPatch, SystemStats, Tag, TestAdapterBody,
    TestAdapterRequest, TriggerScan, TriggerScanProviderId, UpdateProvider, UpsertMapping, UserId,
    UserRow2 as UserRow, VerifyEmailRequest, WatchStatus, WatchlistItem, WatchlistUpsert,
};

// Generated names that read poorly at the call site keep a local alias.
pub(crate) use crate::wire::types::AdminAccountRow as AdminSyncAccount;
pub(crate) use crate::wire::types::AdminMappingRow as AdminSyncMapping;
pub(crate) use crate::wire::types::AuditView as AuditEntry;
pub(crate) use crate::wire::types::FailedTaskView as FailedTask;
pub(crate) use crate::wire::types::MergeCandidateView as MergeCandidate;
pub(crate) use crate::wire::types::RemoteEntryRow as UnmatchedRemoteEntry;
pub(crate) use crate::wire::types::UnmappedSeriesRow as UnmappedSeries;

/// The notifications list is product-defined free-form JSON on the server, so it is untyped
/// here too rather than pretending to a schema the API does not publish.
pub(crate) type Notification = serde_json::Value;

/// The SSE push body: just the recomputed unread count.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct LiveNotification {
    pub(crate) unread_count: i64,
}

/// One page of `GET /v1/series`. The body is a plain `Vec<SeriesSummary>`; the match total
/// and next-page cursor ride on the `X-Total-Count` / `X-Next-Cursor` response headers, so
/// they are stitched back together here rather than being part of the generated type.
#[derive(Debug, Clone)]
pub(crate) struct SeriesPage {
    pub(crate) items: Vec<SeriesSummary>,
    pub(crate) total: i64,
    pub(crate) next_cursor: Option<i64>,
}

pub(crate) trait ContentTypeExt {
    fn label(&self) -> &'static str;
    fn token(&self) -> &'static str;
    /// The accent colour that encodes this type across cards and the series hero.
    fn color(&self) -> &'static str;
    fn all() -> &'static [ContentType];
}

impl ContentTypeExt for ContentType {
    fn label(&self) -> &'static str {
        match self {
            Self::Manga => "Manga",
            Self::Manhwa => "Manhwa",
            Self::Manhua => "Manhua",
            Self::Webtoon => "Webtoon",
            Self::Unknown => "Unknown",
        }
    }
    fn token(&self) -> &'static str {
        match self {
            Self::Manga => "manga",
            Self::Manhwa => "manhwa",
            Self::Manhua => "manhua",
            Self::Webtoon => "webtoon",
            Self::Unknown => "unknown",
        }
    }
    fn color(&self) -> &'static str {
        match self {
            Self::Manga => "var(--color-type-manga)",
            Self::Manhwa => "var(--color-type-manhwa)",
            Self::Manhua => "var(--color-type-manhua)",
            Self::Webtoon => "var(--color-type-webtoon)",
            Self::Unknown => "var(--muted)",
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

pub(crate) trait SeriesStatusExt {
    fn label(&self) -> &'static str;
    fn token(&self) -> &'static str;
    /// The dot colour that encodes this status.
    fn color(&self) -> &'static str;
    fn all() -> &'static [SeriesStatus];
}

impl SeriesStatusExt for SeriesStatus {
    fn label(&self) -> &'static str {
        match self {
            Self::Ongoing => "Ongoing",
            Self::Completed => "Completed",
            Self::Hiatus => "Hiatus",
            Self::Cancelled => "Cancelled",
            Self::Unknown => "Unknown",
        }
    }
    fn token(&self) -> &'static str {
        match self {
            Self::Ongoing => "ongoing",
            Self::Completed => "completed",
            Self::Hiatus => "hiatus",
            Self::Cancelled => "cancelled",
            Self::Unknown => "unknown",
        }
    }
    fn color(&self) -> &'static str {
        match self {
            Self::Ongoing => "var(--color-status-ongoing)",
            Self::Completed => "var(--color-status-completed)",
            Self::Hiatus => "var(--color-status-hiatus)",
            Self::Cancelled | Self::Unknown => "var(--muted)",
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

pub(crate) trait WatchStatusExt {
    fn label(&self) -> &'static str;
    fn token(&self) -> &'static str;
    fn parse(token: &str) -> WatchStatus;
    /// Kanban column order on the Watchlist board.
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
    fn token(&self) -> &'static str {
        match self {
            Self::Reading => "reading",
            Self::Planned => "planned",
            Self::Completed => "completed",
            Self::Dropped => "dropped",
            Self::Paused => "paused",
        }
    }
    fn parse(token: &str) -> WatchStatus {
        match token {
            "planned" => Self::Planned,
            "completed" => Self::Completed,
            "dropped" => Self::Dropped,
            "paused" => Self::Paused,
            _ => Self::Reading,
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

pub(crate) trait RunStateExt {
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

pub(crate) trait ScanRunExt {
    /// Completion in `0.0..=1.0`; zero for a run with no tasks yet, never a division by zero.
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

/// How to settle a local/remote disagreement. The wire carries a bare string (the sync
/// service validates it), so this is the frontend's closed enumeration of the tokens it
/// offers, with the reader-facing wording for each.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ConflictPolicy {
    LocalWins,
    RemoteWins,
    NewestWins,
    AskMe,
}

impl ConflictPolicy {
    pub(crate) const ALL: [ConflictPolicy; 4] = [
        Self::LocalWins,
        Self::RemoteWins,
        Self::NewestWins,
        Self::AskMe,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::LocalWins => "Always keep my local progress",
            Self::RemoteWins => "Always trust AniList",
            Self::NewestWins => "Use whichever changed most recently",
            Self::AskMe => "Ask me when they disagree",
        }
    }

    pub(crate) fn token(self) -> &'static str {
        match self {
            Self::LocalWins => "local_wins",
            Self::RemoteWins => "remote_wins",
            Self::NewestWins => "newest_wins",
            Self::AskMe => "ask_me",
        }
    }

    pub(crate) fn parse(token: &str) -> Self {
        match token {
            "local_wins" => Self::LocalWins,
            "remote_wins" => Self::RemoteWins,
            "ask_me" => Self::AskMe,
            _ => Self::NewestWins,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watch_status_tokens_round_trip() {
        for status in WatchStatus::columns() {
            assert_eq!(WatchStatus::parse(status.token()), *status);
        }
    }

    #[test]
    fn conflict_policy_tokens_round_trip() {
        for policy in ConflictPolicy::ALL {
            assert_eq!(ConflictPolicy::parse(policy.token()), policy);
        }
    }

    #[test]
    fn an_unknown_conflict_policy_falls_back_to_newest_wins() {
        assert_eq!(
            ConflictPolicy::parse("whatever"),
            ConflictPolicy::NewestWins
        );
    }
}
