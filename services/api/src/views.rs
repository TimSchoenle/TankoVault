//! Repository row → wire view conversions: the one seam allowed to know both the
//! `tankovault-db` and `tankovault_contracts` layers, so a renamed or dropped column is a
//! compile error here rather than a silent drift in the published API.
//!
//! Keep conversions exhaustive and literal — no `..Default::default()`, no struct-update
//! syntax — so a new column forces a decision about whether it is published.

use tankovault_contracts::{admin, catalogue, me};
use tankovault_db::repo;

/// A persisted row that has a published counterpart.
pub(crate) trait IntoView {
    /// The wire type this row is published as.
    type View;

    fn into_view(self) -> Self::View;
}

/// Lists convert element-wise, so a handler answering with a page writes `.into_view()` once
/// rather than an `into_iter().map(..).collect()` at every call site.
impl<T: IntoView> IntoView for Vec<T> {
    type View = Vec<T::View>;

    fn into_view(self) -> Self::View {
        self.into_iter().map(IntoView::into_view).collect()
    }
}

/// A wire value accepted in a request body that has a persisted counterpart. The inverse of
/// [`IntoView`], needed only for the two enums a client may send.
pub(crate) trait IntoStored {
    /// The repository type this value is stored as.
    type Stored;

    fn into_stored(self) -> Self::Stored;
}

// --- admin: system overview -------------------------------------------------------------

impl IntoView for repo::stats::SystemStats {
    type View = admin::SystemStatsView;

    fn into_view(self) -> Self::View {
        admin::SystemStatsView {
            providers_total: self.providers_total,
            providers_active: self.providers_active,
            providers_disabled: self.providers_disabled,
            providers_unhealthy: self.providers_unhealthy,
            series_total: self.series_total,
            sources_total: self.sources_total,
            chapters_total: self.chapters_total,
            chapters_1h: self.chapters_1h,
            chapters_24h: self.chapters_24h,
            chapters_7d: self.chapters_7d,
            users_total: self.users_total,
            pending_merges: self.pending_merges,
            runs_active: self.runs_active,
            runs_running: self.runs_running,
            tasks_queued: self.tasks_queued,
            tasks_running: self.tasks_running,
            tasks_failed_24h: self.tasks_failed_24h,
        }
    }
}

impl IntoView for repo::stats::ProviderStat {
    type View = admin::ProviderStatView;

    fn into_view(self) -> Self::View {
        admin::ProviderStatView {
            provider_id: self.provider_id,
            slug: self.slug,
            name: self.name,
            state: self.state,
            adapter: self.adapter,
            series_count: self.series_count,
            source_count: self.source_count,
            blocked_sources: self.blocked_sources,
            chapter_count: self.chapter_count,
            chapters_24h: self.chapters_24h,
            chapters_7d: self.chapters_7d,
            last_chapter_at: self.last_chapter_at,
            last_scanned_at: self.last_scanned_at,
            last_full_scan_at: self.last_full_scan_at,
            last_run_state: self.last_run_state,
            last_run_at: self.last_run_at,
        }
    }
}

impl IntoView for repo::audit::AuditView {
    type View = admin::AuditView;

    fn into_view(self) -> Self::View {
        admin::AuditView {
            id: self.id,
            actor: self.actor,
            action: self.action,
            target: self.target,
            detail: self.detail,
            created_at: self.created_at,
        }
    }
}

impl IntoView for repo::scans::FailedTaskView {
    type View = admin::FailedTaskView;

    fn into_view(self) -> Self::View {
        admin::FailedTaskView {
            id: self.id,
            run_id: self.run_id,
            provider_slug: self.provider_slug,
            mode: self.mode,
            kind: self.kind,
            error: self.error,
            attempts: self.attempts,
            finished_at: self.finished_at,
            acknowledged_at: self.acknowledged_at,
        }
    }
}

impl IntoView for repo::scans::RunListing {
    type View = admin::ScanRunView;

    fn into_view(self) -> Self::View {
        admin::ScanRunView {
            id: self.run.id,
            provider_id: self.run.provider_id,
            provider_slug: self.provider_slug,
            mode: self.run.mode,
            state: self.run.state,
            total_tasks: self.run.total_tasks,
            done_tasks: self.run.done_tasks,
            failed_tasks: self.run.failed_tasks,
            started_at: self.run.started_at,
            finished_at: self.run.finished_at,
            created_at: self.run.created_at,
        }
    }
}

/// The window rollup and its per-provider breakdown are two statements against the same filter,
/// so the pair converts together rather than leaving the handler to assemble a published shape.
impl IntoView
    for (
        repo::scans::ScanSummary,
        Vec<repo::scans::ProviderScanHealth>,
    )
{
    type View = admin::ScanSummaryView;

    fn into_view(self) -> Self::View {
        let (summary, providers) = self;
        admin::ScanSummaryView {
            runs_total: summary.runs_total,
            runs_queued: summary.runs_queued,
            runs_running: summary.runs_running,
            runs_completed: summary.runs_completed,
            runs_failed: summary.runs_failed,
            runs_cancelled: summary.runs_cancelled,
            tasks_total: summary.tasks_total,
            tasks_done: summary.tasks_done,
            tasks_failed: summary.tasks_failed,
            failures_open: summary.failures_open,
            busy_seconds: summary.busy_seconds,
            first_run_at: summary.first_run_at,
            last_run_at: summary.last_run_at,
            providers: providers.into_view(),
        }
    }
}

impl IntoView for repo::scans::ProviderScanHealth {
    type View = admin::ProviderScanHealthView;

    fn into_view(self) -> Self::View {
        admin::ProviderScanHealthView {
            slug: self.slug,
            name: self.name,
            runs: self.runs,
            runs_active: self.runs_active,
            runs_failed: self.runs_failed,
            tasks_done: self.tasks_done,
            tasks_failed: self.tasks_failed,
            failures_open: self.failures_open,
            last_run_at: self.last_run_at,
            last_failure_at: self.last_failure_at,
        }
    }
}

impl IntoView for repo::scans::RunActivity {
    type View = admin::RunActivityView;

    fn into_view(self) -> Self::View {
        admin::RunActivityView {
            run_id: tankovault_domain::ScanRunId::from_uuid(self.run_id),
            queued_tasks: self.queued_tasks,
            running_tasks: self.running_tasks,
            oldest_claim_at: self.oldest_claim_at,
            kinds: self.kinds,
            workers: self.workers,
        }
    }
}

impl IntoView for repo::scans::TaskEvent {
    type View = admin::TaskEventView;

    fn into_view(self) -> Self::View {
        admin::TaskEventView {
            id: self.id,
            run_id: tankovault_domain::ScanRunId::from_uuid(self.run_id),
            provider_slug: self.provider_slug,
            kind: self.kind,
            state: self.state,
            target: self.target,
            error: self.error,
            attempts: self.attempts,
            finished_at: self.finished_at,
        }
    }
}

impl IntoView for repo::matching::MergeCandidateView {
    type View = admin::MergeCandidateView;

    fn into_view(self) -> Self::View {
        admin::MergeCandidateView {
            id: self.id,
            series_id: self.series_id,
            series_title: self.series_title,
            series_sources: self.series_sources,
            series_chapters: self.series_chapters,
            candidate_id: self.candidate_id,
            candidate_title: self.candidate_title,
            candidate_sources: self.candidate_sources,
            candidate_chapters: self.candidate_chapters,
            score: self.score,
            signals: self.signals,
            reason: self.reason,
            suggested_keep: self.suggested_keep,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

impl IntoView for repo::matching::MergeDecisionRow {
    type View = admin::MergeDecisionView;

    fn into_view(self) -> Self::View {
        admin::MergeDecisionView {
            id: self.id,
            decided_at: self.decided_at,
            sweep_id: self.sweep_id,
            trigger: self.trigger,
            actor: self.actor,
            left_id: self.left_id,
            right_id: self.right_id,
            left_title: self.left_title,
            right_title: self.right_title,
            verdict: self.verdict,
            reason: self.reason,
            blocked_by: self.blocked_by,
            outcome: self.outcome,
            survivor_id: self.survivor_id,
            absorbed_id: self.absorbed_id,
            score: self.score,
            base_score: self.base_score,
            signals: self.signals,
            terms: self.terms,
            evidence: self.evidence,
            policy: self.policy,
            revertible: self.revertible,
            undo_rows: self.undo_rows,
            reverted_at: self.reverted_at,
            reverted_by: self.reverted_by,
            revert_reason: self.revert_reason,
            flagged_at: self.flagged_at,
            flagged_by: self.flagged_by,
            flag_reason: self.flag_reason,
        }
    }
}

impl IntoView for repo::sync::SyncDecisionRow {
    type View = admin::SyncDecisionView;

    fn into_view(self) -> Self::View {
        admin::SyncDecisionView {
            id: self.id,
            run_id: self.run_id,
            decided_at: self.decided_at,
            user_id: self.user_id,
            username: self.username,
            series_id: self.series_id,
            series_title: self.series_title,
            provider: self.provider,
            external_id: self.external_id,
            scope: self.scope,
            action: self.action,
            reason: self.reason,
            policy: self.policy,
            applied: self.applied,
            local_before: self.local_before,
            local_after: self.local_after,
            remote_before: self.remote_before,
            remote_after: self.remote_after,
            ancestor_local: self.ancestor_local,
            ancestor_remote: self.ancestor_remote,
            match_score: self.match_score,
            match_signals: self.match_signals,
            evidence: self.evidence,
            reverted_at: self.reverted_at,
            reverted_by: self.reverted_by,
            revert_reason: self.revert_reason,
            flagged_at: self.flagged_at,
            flagged_by: self.flagged_by,
            flag_reason: self.flag_reason,
        }
    }
}

// --- admin: external sync ---------------------------------------------------------------

impl IntoView for repo::sync::AdminAccountRow {
    type View = admin::SyncAccountView;

    fn into_view(self) -> Self::View {
        admin::SyncAccountView {
            user_id: self.user_id,
            username: self.username,
            provider: self.provider,
            external_username: self.external_username,
            last_synced_at: self.last_synced_at,
            last_error: self.last_error,
            auto_sync_enabled: self.auto_sync_enabled,
            conflict_policy: self.conflict_policy,
            pending_conflicts: self.pending_conflicts,
            created_at: self.created_at,
        }
    }
}

impl IntoView for repo::sync::AdminMappingRow {
    type View = admin::SyncMappingView;

    fn into_view(self) -> Self::View {
        admin::SyncMappingView {
            series_id: self.series_id,
            series_title: self.series_title,
            provider: self.provider,
            external_id: self.external_id,
            updated_at: self.updated_at,
        }
    }
}

impl IntoView for repo::sync::UnmappedSeriesRow {
    type View = admin::UnmappedSeriesView;

    fn into_view(self) -> Self::View {
        admin::UnmappedSeriesView {
            series_id: self.series_id,
            series_title: self.series_title,
            source_count: self.source_count,
        }
    }
}

impl IntoView for repo::sync::RemoteEntryRow {
    type View = admin::RemoteEntryView;

    fn into_view(self) -> Self::View {
        admin::RemoteEntryView {
            user_id: self.user_id,
            username: self.username,
            provider: self.provider,
            external_id: self.external_id,
            title: self.title,
            status: self.status,
            progress: self.progress,
            content_type: self.content_type,
            start_year: self.start_year,
        }
    }
}

// --- admin: user directory --------------------------------------------------------------

impl IntoView for repo::user_admin::DirectoryRow {
    type View = admin::UserDirectoryRow;

    fn into_view(self) -> Self::View {
        admin::UserDirectoryRow {
            id: self.id,
            email: self.email,
            username: self.username,
            status: self.status,
            email_verified: self.email_verified,
            permission_count: self.permission_count,
            is_super_user: self.is_super_user,
            tracked_count: self.tracked_count,
            last_login_at: self.last_login_at,
            created_at: self.created_at,
        }
    }
}

impl IntoView for repo::scans::RunPage {
    type View = admin::ScanRunPageView;

    fn into_view(self) -> Self::View {
        admin::ScanRunPageView {
            items: self.items.into_view(),
            total: self.total,
        }
    }
}

impl IntoView for repo::scans::FailureGroup {
    type View = admin::FailureGroupView;

    fn into_view(self) -> Self::View {
        admin::FailureGroupView {
            error: self.error,
            count: self.count,
            cleared: self.cleared,
            providers: self.providers,
            kinds: self.kinds,
            latest_at: self.latest_at,
        }
    }
}

impl IntoView for repo::audit::AuditPage {
    type View = admin::AuditPageView;

    fn into_view(self) -> Self::View {
        admin::AuditPageView {
            items: self.items.into_view(),
            total: self.total,
        }
    }
}

impl IntoView for repo::user_admin::DirectoryPage {
    type View = admin::UserDirectoryPage;

    fn into_view(self) -> Self::View {
        admin::UserDirectoryPage {
            users: self.users.into_view(),
            total: self.total,
        }
    }
}

impl IntoView for repo::user_admin::UserDetail {
    type View = admin::UserDetailView;

    fn into_view(self) -> Self::View {
        admin::UserDetailView {
            id: self.id,
            email: self.email,
            username: self.username,
            status: self.status,
            email_verified: self.email_verified,
            suspension_reason: self.suspension_reason,
            suspended_at: self.suspended_at,
            last_login_at: self.last_login_at,
            created_at: self.created_at,
            active_sessions: self.active_sessions,
            tracked_count: self.tracked_count,
            linked_accounts: self.linked_accounts,
            open_privacy_requests: self.open_privacy_requests,
        }
    }
}

impl IntoView for repo::permissions::GrantRow {
    type View = admin::GrantView;

    fn into_view(self) -> Self::View {
        admin::GrantView {
            permission: self.permission,
            known: self.known,
            granted_at: self.granted_at,
            granted_by: self.granted_by,
        }
    }
}

// --- privacy: the data-subject request queue --------------------------------------------
//
// The two enums are mirrored rather than moved: their `sqlx::Type` derive binds them to the
// `gdpr_request_kind` / `gdpr_request_status` SQL enums, and `tankovault-contracts` must not
// depend on a database driver. Both directions are written out and both matches are
// exhaustive, so adding a variant on either side fails to compile until it is mapped.

impl IntoView for repo::gdpr::RequestKind {
    type View = me::PrivacyRequestKind;

    fn into_view(self) -> Self::View {
        match self {
            Self::Access => me::PrivacyRequestKind::Access,
            Self::Portability => me::PrivacyRequestKind::Portability,
            Self::Rectification => me::PrivacyRequestKind::Rectification,
            Self::Erasure => me::PrivacyRequestKind::Erasure,
            Self::Restriction => me::PrivacyRequestKind::Restriction,
            Self::Objection => me::PrivacyRequestKind::Objection,
        }
    }
}

impl IntoStored for me::PrivacyRequestKind {
    type Stored = repo::gdpr::RequestKind;

    fn into_stored(self) -> Self::Stored {
        match self {
            Self::Access => repo::gdpr::RequestKind::Access,
            Self::Portability => repo::gdpr::RequestKind::Portability,
            Self::Rectification => repo::gdpr::RequestKind::Rectification,
            Self::Erasure => repo::gdpr::RequestKind::Erasure,
            Self::Restriction => repo::gdpr::RequestKind::Restriction,
            Self::Objection => repo::gdpr::RequestKind::Objection,
        }
    }
}

impl IntoView for repo::gdpr::RequestStatus {
    type View = me::PrivacyRequestStatus;

    fn into_view(self) -> Self::View {
        match self {
            Self::Pending => me::PrivacyRequestStatus::Pending,
            Self::InProgress => me::PrivacyRequestStatus::InProgress,
            Self::Completed => me::PrivacyRequestStatus::Completed,
            Self::Rejected => me::PrivacyRequestStatus::Rejected,
            Self::Cancelled => me::PrivacyRequestStatus::Cancelled,
        }
    }
}

impl IntoStored for me::PrivacyRequestStatus {
    type Stored = repo::gdpr::RequestStatus;

    fn into_stored(self) -> Self::Stored {
        match self {
            Self::Pending => repo::gdpr::RequestStatus::Pending,
            Self::InProgress => repo::gdpr::RequestStatus::InProgress,
            Self::Completed => repo::gdpr::RequestStatus::Completed,
            Self::Rejected => repo::gdpr::RequestStatus::Rejected,
            Self::Cancelled => repo::gdpr::RequestStatus::Cancelled,
        }
    }
}

impl IntoView for repo::gdpr::RequestRow {
    type View = me::PrivacyRequestView;

    fn into_view(self) -> Self::View {
        me::PrivacyRequestView {
            id: self.id,
            kind: self.kind.into_view(),
            status: self.status.into_view(),
            detail: self.detail,
            requested_at: self.requested_at,
            due_at: self.due_at,
            resolved_at: self.resolved_at,
            resolution_note: self.resolution_note,
        }
    }
}

impl IntoView for repo::gdpr::AdminRequestRow {
    type View = admin::AdminPrivacyRequestView;

    fn into_view(self) -> Self::View {
        admin::AdminPrivacyRequestView {
            // Read before `request` is moved into the view: the kind is what decides this, and
            // `RequestKind::needs_export` is the one definition of it (FRONTEND F10).
            needs_export: self.request.kind.needs_export(),
            request: self.request.into_view(),
            user_id: self.user_id,
            username: self.username,
            email: self.email,
            claimed_by: self.claimed_by,
            resolved_by: self.resolved_by,
            overdue: self.overdue,
        }
    }
}

// --- the signed-in user's own surface ---------------------------------------------------

impl IntoView for repo::tracking::MeStats {
    type View = me::MeStatsView;

    fn into_view(self) -> Self::View {
        me::MeStatsView {
            tracking: self.tracking,
            reading: self.reading,
            completed: self.completed,
            chapters_read: self.chapters_read,
            unread: self.unread,
        }
    }
}

// --- public catalogue -------------------------------------------------------------------

impl IntoView for repo::providers::PublicProvider {
    type View = catalogue::PublicProviderView;

    fn into_view(self) -> Self::View {
        catalogue::PublicProviderView {
            id: self.id,
            slug: self.slug,
            name: self.name,
            series_count: self.series_count,
        }
    }
}
