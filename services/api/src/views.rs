//! Repository row → wire view conversions.
//!
//! # Why this module exists
//!
//! `tankovault-db` used to derive `utoipa::ToSchema` on 23 repository row structs, and 11
//! handlers returned those rows verbatim. The persistence layer was therefore the public HTTP
//! schema: renaming a column in a `SELECT` rewrote the published API and the generated
//! `crates/api-client` with **no compile error anywhere**, because no handler ever named a
//! field. That is a breaking change that could not be caught by reviewing this crate.
//!
//! The wire shapes now live in `tankovault_contracts::{admin, catalogue, me}` and the rows are
//! plain query results again. This module is the seam: `services/api` is the only crate
//! permitted to know both layers, so every mapping is written out exactly once, here. A
//! renamed or dropped column is now a compile error in this file and nowhere else.
//!
//! # Why a trait rather than `From`
//!
//! Both sides are foreign types, so `impl From<db::Row> for contracts::View` is barred by the
//! orphan rule. [`IntoView`] and [`IntoStored`] are local traits, which is what makes the
//! conversions legal here — and they carry their direction in the name, which `From` would
//! not.
//!
//! Keep the conversions exhaustive and literal — no `..Default::default()`, no struct-update
//! syntax. The point is that adding a column to a row must force a decision about whether it
//! is published.

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
            candidate_id: self.candidate_id,
            candidate_title: self.candidate_title,
            score: self.score,
            reason: self.reason,
            created_at: self.created_at,
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
            tracked_count: self.tracked_count,
            last_login_at: self.last_login_at,
            created_at: self.created_at,
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
