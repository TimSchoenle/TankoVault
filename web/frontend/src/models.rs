//! Wire DTOs and the presentation-only helpers hung off them.
//!
//! Every request and response shape is generated at compile time from the API service's
//! `utoipa` schema (`xtask openapi` → `progenitor` → `tankovault-api-client`). This module
//! re-exports those types and adds labelling/ordering helpers that don't belong in generated code.
//!
//! Nothing here hand-mirrors a payload: a hand-mirrored shape can drift from the server
//! silently, discarding persisted values or renaming fields out from under a view.

use serde::{Deserialize, Serialize};

pub(crate) use crate::wire::types::{
    AccountStatus, AdapterKind, AssignRemoteEntry, ChapterDto, ChapterRead, ConflictPolicy,
    ConflictRow, ContentType, ContinueItem, CreateProvider, DismissRequest, FeedEntry,
    ForgotPasswordRequest, LoginRequest, MarkRead, MarkReadTo, MergeRequest, PasskeyLoginRequest,
    PermissionPreset, Politeness, PolitenessEmulation, ProblemDetails, ProfileUpdate, ProgressDto,
    ProgressUpdate, Provider, ProviderId, ProviderInfo, ProviderStat, ProviderState,
    PublicProvider, RegisterRequest, RequestKind, RequestStatus, ResendVerificationRequest,
    ResetPasswordRequest, ResolveConflict, RunState, ScanMode, ScanRun, ScanRunProviderId,
    SeriesDetail, SeriesId, SeriesSourceId, SeriesStatus, SeriesSummary,
    SetProviderState as SetProviderStateBody, SourceDto, SuggestedMatch, SyncExcluded, SyncOpts,
    SyncPullBody, SyncPushBody, SyncSettingsPatch, SystemStats, Tag, TestAdapterBody,
    TestAdapterRequest, TriggerScan, TriggerScanProviderId, UpdateProvider, UpsertMapping, UserId,
    VerifyEmailRequest, WatchStatus, WatchlistBulkIds, WatchlistBulkUpdate, WatchlistCounts,
    WatchlistEntryViewEntry, WatchlistGroup, WatchlistItem, WatchlistUpsert, WatchlistView,
};

// `BulkResult` says nothing about what it is a result *of*; only the watchlist bulk bar and
// the group-header mark-read call it.
pub(crate) use crate::wire::types::BulkResult;

// `SyncAccountStatus` (external-tracker link status) keeps its generated name so it can't be
// confused with `AccountStatus` (user account active/suspended).
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
    /// Every status, in the order the Watchlist's tab strip and its movers offer them:
    /// the shelf a title is on, then the shelves it plausibly moves to.
    ///
    /// Deliberately no `parse` beside it: mapping an unrecognised token to `Reading` would
    /// invent a status the caller never named and silently hide most of the watchlist filter.
    /// Callers match on `token()` and handle the miss themselves.
    fn all() -> &'static [WatchStatus];
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
    fn all() -> &'static [WatchStatus] {
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
    // `AdminRequestRow.needs_export` carries this answer already; don't re-derive it here.
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
            // Amber, not vermilion: a suspension is reversible, not a failure.
            Self::Suspended => "ik-pill star",
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

/// Presentation for the generated [`ConflictPolicy`].
pub(crate) trait ConflictPolicyExt: Sized {
    /// Every policy, in the order the picker offers them.
    ///
    /// Still hand-listed, because the generated client carries no `ALL`, but kept honest by
    /// `the_policy_picker_offers_every_published_policy`, which reads the accepted set out of
    /// the committed `openapi.json`.
    fn all() -> &'static [Self];
    /// The catalogue key of this policy's display name (see [`crate::i18n`]).
    fn label_key(self) -> &'static str;
}

impl ConflictPolicyExt for ConflictPolicy {
    fn all() -> &'static [Self] {
        &[
            Self::LocalWins,
            Self::RemoteWins,
            Self::NewestWins,
            Self::AskMe,
        ]
    }

    fn label_key(self) -> &'static str {
        match self {
            Self::LocalWins => "enum.conflictPolicy.localWins",
            Self::RemoteWins => "enum.conflictPolicy.remoteWins",
            Self::NewestWins => "enum.conflictPolicy.newestWins",
            Self::AskMe => "enum.conflictPolicy.askMe",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tokens are the wire contract with `tankovault_domain::WatchStatus` and with the
    /// watchlist's `?status=` parameter, and callers select a status by comparing against
    /// them. Two variants sharing a token would make one of them unreachable from a URL and
    /// unpickable from the bulk bar — silently, and only for whichever the `find` hit second.
    #[test]
    fn every_watch_status_has_a_distinct_token() {
        let mut tokens: Vec<&str> = WatchStatus::all()
            .iter()
            .map(super::WatchStatusExt::token)
            .collect();
        let listed = tokens.len();
        tokens.sort_unstable();
        tokens.dedup();
        assert_eq!(tokens.len(), listed, "two watch statuses share a token");
        assert_eq!(listed, 5, "a status is missing from WatchStatus::all()");
    }

    /// The policy picker must offer every policy the server accepts.
    ///
    /// `ConflictPolicy::all()` is the last hand-maintained list in this file. Read against the
    /// committed `openapi.json` — the artefact `crates/api-client` is generated from — so a
    /// policy added to `tankovault_contracts::sync::ConflictPolicy` fails here rather than
    /// quietly never appearing in the UI.
    #[test]
    fn the_policy_picker_offers_every_published_policy() {
        const SPEC: &str = include_str!("../../../openapi.json");
        let spec: serde_json::Value = serde_json::from_str(SPEC).expect("openapi.json parses");

        let mut published: Vec<String> = spec["components"]["schemas"]["ConflictPolicy"]["enum"]
            .as_array()
            .expect("the document declares the ConflictPolicy vocabulary")
            .iter()
            .map(|v| v.as_str().expect("policy tokens are strings").to_owned())
            .collect();
        let mut offered: Vec<String> = ConflictPolicy::all()
            .iter()
            .map(ToString::to_string)
            .collect();

        assert_eq!(
            offered.len(),
            ConflictPolicy::all().len(),
            "a policy is listed twice in ConflictPolicy::all()"
        );
        published.sort();
        offered.sort();
        assert_eq!(
            offered, published,
            "the account Sync panel offers a different set of conflict policies than the API \
             publishes; add the missing variant to `ConflictPolicy::all()` and word it in \
             `label_key`"
        );
    }
}
