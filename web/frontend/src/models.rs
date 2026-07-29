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
    LoginRequest, MarkRead, MergeRequest, PermissionPreset, Politeness, PolitenessEmulation,
    ProfileUpdate, ProgressUpdate, Provider, ProviderId, ProviderInfo, ProviderStat, ProviderState,
    PublicProvider, RegisterRequest, RequestKind, RequestStatus, ResendVerificationRequest,
    ResetPasswordRequest, ResolveConflict, RunState, ScanMode, ScanRun, ScanRunProviderId,
    SeriesId, SeriesSourceId, SeriesStatus, SeriesSummary,
    SetProviderState as SetProviderStateBody, SourceDto, SuggestedMatch, SyncExcluded, SyncOpts,
    SyncPullBody, SyncPushBody, SyncSettingsPatch, SystemStats, Tag, TestAdapterBody,
    TestAdapterRequest, TriggerScan, TriggerScanProviderId, UpdateProvider, UpsertMapping, UserId,
    VerifyEmailRequest, WatchStatus, WatchlistItem, WatchlistUpsert,
};

// Generated names that read poorly at the call site keep a local alias.
//
// `SyncAccountStatus` is the exception that reads *better* generated than aliased: it is the
// external-tracker link status, and it is qualified precisely because `AccountStatus` is now a
// different thing — whether a user account is active or suspended. Both are re-exported above
// and below under names that cannot be confused.
pub(crate) use crate::wire::types::SyncAccountStatus;

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
    /// The catalogue key of this variant's display name (see [`crate::i18n`]).
    fn label_key(&self) -> &'static str;
    fn token(&self) -> &'static str;
    /// The accent colour that encodes this type across cards and the series hero.
    fn color(&self) -> &'static str;
    fn all() -> &'static [ContentType];
}

impl ContentTypeExt for ContentType {
    fn label_key(&self) -> &'static str {
        match self {
            Self::Manga => "enum.contentType.manga",
            Self::Manhwa => "enum.contentType.manhwa",
            Self::Manhua => "enum.contentType.manhua",
            Self::Webtoon => "enum.contentType.webtoon",
            Self::Unknown => "enum.contentType.unknown",
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
    /// The catalogue key of this variant's display name (see [`crate::i18n`]).
    fn label_key(&self) -> &'static str;
    fn token(&self) -> &'static str;
    /// The dot colour that encodes this status.
    fn color(&self) -> &'static str;
    fn all() -> &'static [SeriesStatus];
}

impl SeriesStatusExt for SeriesStatus {
    fn label_key(&self) -> &'static str {
        match self {
            Self::Ongoing => "enum.seriesStatus.ongoing",
            Self::Completed => "enum.seriesStatus.completed",
            Self::Hiatus => "enum.seriesStatus.hiatus",
            Self::Cancelled => "enum.seriesStatus.cancelled",
            Self::Unknown => "enum.seriesStatus.unknown",
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
    /// The catalogue key of this variant's display name (see [`crate::i18n`]).
    fn label_key(&self) -> &'static str;
    fn token(&self) -> &'static str;
    fn parse(token: &str) -> WatchStatus;
    /// Kanban column order on the Watchlist board.
    fn columns() -> &'static [WatchStatus];
}

impl WatchStatusExt for WatchStatus {
    fn label_key(&self) -> &'static str {
        match self {
            Self::Reading => "enum.watchStatus.reading",
            Self::Planned => "enum.watchStatus.planned",
            Self::Completed => "enum.watchStatus.completed",
            Self::Dropped => "enum.watchStatus.dropped",
            Self::Paused => "enum.watchStatus.paused",
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

pub(crate) trait RequestKindExt {
    /// The catalogue key of this variant's display name (see [`crate::i18n`]).
    fn label_key(self) -> &'static str;
    /// The wire token, used as a `<select>` option value.
    fn token(self) -> &'static str;
    /// Whether fulfilling this kind means disclosing the subject's export.
    ///
    /// Mirrors `RequestKind::needs_export` on the server. Duplicated rather than shared because
    /// the generated client carries no methods; the server refuses the call regardless, so this
    /// only decides whether the button is worth offering.
    fn needs_export(self) -> bool;
    /// Every kind, in the order the request form offers them.
    fn all() -> &'static [RequestKind];
}

impl RequestKindExt for RequestKind {
    fn label_key(self) -> &'static str {
        match self {
            Self::Access => "enum.privacyKind.access",
            Self::Portability => "enum.privacyKind.portability",
            Self::Rectification => "enum.privacyKind.rectification",
            Self::Erasure => "enum.privacyKind.erasure",
            Self::Restriction => "enum.privacyKind.restriction",
            Self::Objection => "enum.privacyKind.objection",
        }
    }
    fn token(self) -> &'static str {
        match self {
            Self::Access => "access",
            Self::Portability => "portability",
            Self::Rectification => "rectification",
            Self::Erasure => "erasure",
            Self::Restriction => "restriction",
            Self::Objection => "objection",
        }
    }
    fn needs_export(self) -> bool {
        matches!(self, Self::Access | Self::Portability)
    }
    fn all() -> &'static [RequestKind] {
        &[
            RequestKind::Access,
            RequestKind::Portability,
            RequestKind::Rectification,
            RequestKind::Erasure,
            RequestKind::Restriction,
            RequestKind::Objection,
        ]
    }
}

pub(crate) trait RequestStatusExt {
    /// The catalogue key of this variant's display name (see [`crate::i18n`]).
    fn label_key(self) -> &'static str;
    /// Whether the request is still awaiting a resolution — the only state in which either
    /// side can still act on it.
    fn is_open(&self) -> bool;
}

impl RequestStatusExt for RequestStatus {
    fn label_key(self) -> &'static str {
        match self {
            Self::Pending => "enum.privacyStatus.pending",
            Self::InProgress => "enum.privacyStatus.inProgress",
            Self::Completed => "enum.privacyStatus.completed",
            Self::Rejected => "enum.privacyStatus.rejected",
            Self::Cancelled => "enum.privacyStatus.cancelled",
        }
    }
    fn is_open(&self) -> bool {
        matches!(self, Self::Pending | Self::InProgress)
    }
}

pub(crate) trait AccountStatusExt {
    /// The catalogue key of this variant's display name (see [`crate::i18n`]).
    fn label_key(self) -> &'static str;
    /// The pill class encoding it: a suspended account must be impossible to skim past.
    fn pill_class(self) -> &'static str;
}

impl AccountStatusExt for AccountStatus {
    fn label_key(self) -> &'static str {
        match self {
            Self::Active => "enum.accountStatus.active",
            Self::Suspended => "enum.accountStatus.suspended",
        }
    }
    fn pill_class(self) -> &'static str {
        match self {
            Self::Active => "ik-pill jade",
            Self::Suspended => "ik-pill vermilion",
        }
    }
}

pub(crate) trait PermissionPresetExt {
    /// The catalogue key of this preset's display name (see [`crate::i18n`]).
    fn label_key(self) -> &'static str;
}

impl PermissionPresetExt for PermissionPreset {
    fn label_key(self) -> &'static str {
        match self {
            Self::Reader => "console.preset.reader",
            Self::Operator => "console.preset.operator",
            Self::Administrator => "console.preset.administrator",
        }
    }
}

pub(crate) trait RunStateExt {
    /// The catalogue key of this variant's display name (see [`crate::i18n`]).
    fn label_key(&self) -> &'static str;
}

impl RunStateExt for RunState {
    fn label_key(&self) -> &'static str {
        match self {
            Self::Queued => "enum.runState.queued",
            Self::Running => "enum.runState.running",
            Self::Completed => "enum.runState.completed",
            Self::Failed => "enum.runState.failed",
            Self::Cancelled => "enum.runState.cancelled",
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
/// offers, each pointing at the catalogue entry that words it for the reader.
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

    /// The catalogue key of this policy's display name (see [`crate::i18n`]).
    pub(crate) fn label_key(self) -> &'static str {
        match self {
            Self::LocalWins => "enum.conflictPolicy.localWins",
            Self::RemoteWins => "enum.conflictPolicy.remoteWins",
            Self::NewestWins => "enum.conflictPolicy.newestWins",
            Self::AskMe => "enum.conflictPolicy.askMe",
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
