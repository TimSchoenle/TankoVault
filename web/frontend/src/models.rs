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
    AccountStatus, AdapterKind, AssignRemoteEntry, BrandingView, ChapterDto, ChapterRead,
    ConflictPolicy, ConflictRow, ContentType, ContinueItem, CreateProvider, DismissRequest,
    FeedEntry, FeedbackBody, ForgotPasswordRequest, LegalDocumentView, LegalIndexEntry, LegalKind,
    LoginRequest, MarkRead, MarkReadTo, MergeRequest, NotificationsView, PasskeyLoginRequest,
    PermissionPreset, PolitenessInput, PreferredProvider, ProblemDetails, ProfileUpdate,
    ProgressDto, ProgressUpdate, Provider, ProviderId, ProviderInfo, ProviderStat, ProviderState,
    PublicProvider, Recommendation, RegisterRequest, RequestKind, RequestStatus,
    ResendVerificationRequest, ResetPasswordRequest, ResolveConflict, RunState, ScanMode, ScanRun,
    SeriesDetail, SeriesId, SeriesSourceId, SeriesStatus, SeriesSummary,
    SetProviderState as SetProviderStateBody, SimilarSeries, SourceDto, SourcePin,
    SourcePreferencesUpdate, SuggestedMatch, SyncExcluded, SyncOpts, SyncSettingsPatch,
    SystemStats, TagFacet, TasteFeature, TasteView, TestAdapterRequest, TriggerScan,
    UpdateProvider, UpsertMapping, UserId, VerifyEmailRequest, WatchStatus, WatchlistBulkIds,
    WatchlistBulkUpdate, WatchlistCounts, WatchlistGroup, WatchlistItem, WatchlistSource,
    WatchlistUpsert, WatchlistView,
};

// `BulkResult` says nothing about what it is a result *of*; only the watchlist bulk bar and
// the group-header mark-read call it.
pub(crate) use crate::wire::types::BulkResult;

pub(crate) use crate::wire::types::{
    PresetDefinition, PresetLink, SetPresetLock as SetPresetLockBody,
};

// `SyncAccountStatus` (external-tracker link status) keeps its generated name so it can't be
// confused with `AccountStatus` (user account active/suspended).
pub(crate) use crate::wire::types::SyncAccountStatus;

pub(crate) use crate::wire::types::AdminAccountRow as AdminSyncAccount;
pub(crate) use crate::wire::types::AdminMappingRow as AdminSyncMapping;
pub(crate) use crate::wire::types::AuditView as AuditEntry;
pub(crate) use crate::wire::types::FailedTaskView as FailedTask;
// The scan console's own vocabulary. `ListScansSort` is progenitor's name for the run-history
// `sort` parameter; `RunSort` is what the panel and the repository both call it.
pub(crate) use crate::wire::types::MergeCandidateView as MergeCandidate;
pub(crate) use crate::wire::types::{
    CancelScansBody, ClearFailuresBody, FailureGroupView as FailureGroup, ListScansSort as RunSort,
    ProviderScanHealthView, RunActivityView as RunActivity, RunTelemetryView as RunTelemetry,
    ScanActivityView as ScanActivity, ScanRunDetailView as ScanRunDetail, ScanRunId,
    ScanSummaryView as ScanSummary, ScanTaskDetailView as ScanTaskDetail,
    StageTotalView as StageTotal, TaskEventView as TaskEvent, TaskState,
};
// The two decision journals keep their generated names: `MergeDecision` is what the operator
// console calls the row, and there is no shorter name that stays distinct from `MergeCandidate`.
pub(crate) use crate::wire::types::RemoteEntryRow as UnmatchedRemoteEntry;
pub(crate) use crate::wire::types::UnmappedSeriesRow as UnmappedSeries;
pub(crate) use crate::wire::types::{MergeDecision, SyncDecision};

/// One inbox row.
///
/// Was `serde_json::Value` while the API published the stored document verbatim, which is why
/// every row read "new chapter": the view had to guess at a payload whose title field no writer
/// ever set. The server resolves the display fields now, so this is a real type.
pub(crate) use crate::wire::types::NotificationItem as Notification;

pub(crate) use crate::wire::types::NotificationPrefs;

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
    /// The ink this type is set in. Deliberately the same muted stop for every variant: colour
    /// encodes health across the app, and [`ContentTypeExt::initial`] is what tells the types
    /// apart.
    fn color(&self) -> &'static str;
    /// The two-letter mono mark that identifies this type without spending a hue on it.
    fn initial(&self) -> &'static str;
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
    // Manhwa and manhua both begin "manh", so their marks are taken from the syllable that
    // differs rather than from the first two letters, which would collide.
    fn initial(&self) -> &'static str {
        match self {
            Self::Manga => "MG",
            Self::Manhwa => "HW",
            Self::Manhua => "HU",
            Self::Webtoon => "WT",
            Self::Unknown => "??",
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
    /// The ink this status is set in — muted for every variant, because series status is prose
    /// about a title rather than a signal an operator scans for.
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

/// Presentation for the generated [`TaskState`].
pub(crate) trait TaskStateExt {
    /// The catalogue key of this variant's display name (see [`crate::i18n`]).
    fn label_key(self) -> &'static str;
    /// Whether this state means the task did *not* do its work.
    fn is_failure(&self) -> bool;
}

impl TaskStateExt for TaskState {
    fn label_key(self) -> &'static str {
        match self {
            Self::Queued => "enum.taskState.queued",
            Self::Claimed => "enum.taskState.claimed",
            Self::Running => "enum.taskState.running",
            Self::Done => "enum.taskState.done",
            Self::Failed => "enum.taskState.failed",
            Self::Skipped => "enum.taskState.skipped",
        }
    }

    fn is_failure(&self) -> bool {
        *self == Self::Failed
    }
}

/// Presentation for the generated run-history ordering.
pub(crate) trait RunSortExt: Sized {
    /// Every ordering, in the order the picker offers them.
    ///
    /// Hand-listed because the generated client carries no `ALL`, and kept honest by
    /// `the_sort_picker_offers_every_published_ordering`, which reads the accepted set out of
    /// the committed `openapi.json`.
    fn all() -> &'static [Self];
    /// The catalogue key of this ordering's display name (see [`crate::i18n`]).
    fn label_key(self) -> &'static str;
    /// The `?sort=` token, which is also what the API accepts.
    fn token(self) -> &'static str;
    /// Parse a token back. An unrecognised one is the default rather than an error: a
    /// hand-edited URL should still open the panel.
    fn parse(token: &str) -> Self;
}

impl RunSortExt for RunSort {
    fn all() -> &'static [Self] {
        &[Self::Recent, Self::Oldest, Self::Failures, Self::Duration]
    }

    fn label_key(self) -> &'static str {
        match self {
            Self::Recent => "console.scan.sort.recent",
            Self::Oldest => "console.scan.sort.oldest",
            Self::Failures => "console.scan.sort.failures",
            Self::Duration => "console.scan.sort.duration",
        }
    }

    fn token(self) -> &'static str {
        match self {
            Self::Recent => "recent",
            Self::Oldest => "oldest",
            Self::Failures => "failures",
            Self::Duration => "duration",
        }
    }

    fn parse(token: &str) -> Self {
        <Self as RunSortExt>::all()
            .iter()
            .copied()
            .find(|sort| sort.token() == token)
            .unwrap_or(Self::Recent)
    }
}

/// Presentation for the generated [`ScanMode`].
pub(crate) trait ScanModeExt: Sized {
    /// Every mode, in the order the picker offers them.
    fn all() -> &'static [Self];
    /// The catalogue key of this mode's display name (see [`crate::i18n`]).
    fn label_key(self) -> &'static str;
    /// The `?mode=` token, which is also what the API accepts.
    fn token(self) -> &'static str;
    /// Parse a token back, or `None` for "any mode".
    fn parse(token: &str) -> Option<Self>;
}

impl ScanModeExt for ScanMode {
    fn all() -> &'static [Self] {
        &[Self::Fast, Self::Full]
    }

    fn label_key(self) -> &'static str {
        match self {
            Self::Fast => "console.scans.modeFast",
            Self::Full => "console.scans.modeFull",
        }
    }

    fn token(self) -> &'static str {
        match self {
            Self::Fast => "fast",
            Self::Full => "full",
        }
    }

    fn parse(token: &str) -> Option<Self> {
        <Self as ScanModeExt>::all()
            .iter()
            .copied()
            .find(|mode| mode.token() == token)
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

    /// The role tokens once drew twenty values across four axes from five literal hues, so
    /// `#6fa8dc` was `manga`, `completed`, `solving` and `running` at the same time and a console
    /// row's colour could not be read without the label beside it. Colour encodes health alone
    /// now: every role token points at a `--color-health-*` stop or at `--muted`, and shape and
    /// letter carry the other three axes. A token re-pointed at a literal of its own — the
    /// obvious way to "tell them apart" again — is that bug returning.
    #[test]
    fn no_role_token_carries_a_colour_of_its_own() {
        const SHEET: &str = include_str!("../input.css");
        const AXES: [&str; 4] = [
            "--color-type-",
            "--color-status-",
            "--color-state-",
            "--color-run-",
        ];

        let mut seen = 0;
        for line in SHEET.lines() {
            let line = line.trim();
            let Some((name, value)) = line.split_once(':') else {
                continue;
            };
            if !AXES.iter().any(|axis| name.starts_with(axis)) {
                continue;
            }
            seen += 1;
            assert!(
                value.contains("var(--"),
                "{name} carries a colour of its own; point it at a --color-health-* stop or at \
                 --muted instead"
            );
        }
        assert_eq!(seen, 18, "a role token was added or dropped without review");
    }

    /// Content types are told apart by their mark, not by a hue, so two sharing one mark would
    /// make them indistinguishable rather than merely hard to tell apart.
    #[test]
    fn every_content_type_has_a_distinct_mark() {
        let mut marks: Vec<&str> = ContentType::all()
            .iter()
            .map(super::ContentTypeExt::initial)
            .collect();
        let listed = marks.len();
        marks.sort_unstable();
        marks.dedup();
        assert_eq!(marks.len(), listed, "two content types share a mark");
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
