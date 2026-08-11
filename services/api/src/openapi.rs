//! The `OpenApi` root. Every `#[utoipa::path(..)]` handler across the crate is collected here
//! via `utoipa_axum`, and `xtask openapi` serialises the result to `openapi.json` to
//! regenerate the typed frontend client.

use utoipa::openapi::security::{ApiKey, ApiKeyValue, Http, HttpAuthScheme, SecurityScheme};
use utoipa::{Modify, OpenApi};

pub const AUTH_TAG: &str = "auth";
pub const SERIES_TAG: &str = "series";
pub const LEGAL_TAG: &str = "legal";
pub const BRANDING_TAG: &str = "branding";
pub const ME_WATCHLIST_TAG: &str = "me-watchlist";
pub const ME_PROGRESS_TAG: &str = "me-progress";
pub const ME_DASHBOARD_TAG: &str = "me-dashboard";
pub const ME_ACCOUNT_TAG: &str = "me-account";
pub const ME_NOTIFICATIONS_TAG: &str = "me-notifications";
pub const ME_SYNC_TAG: &str = "me-sync";
pub const ADMIN_PROVIDERS_TAG: &str = "admin-providers";
pub const ADMIN_SCANS_TAG: &str = "admin-scans";
pub const ADMIN_MATCHING_TAG: &str = "admin-matching";
pub const ADMIN_CATALOGUE_TAG: &str = "admin-catalogue";
pub const ADMIN_SYNC_TAG: &str = "admin-sync";
pub const ADMIN_USERS_TAG: &str = "admin-users";
pub const ADMIN_PRIVACY_TAG: &str = "admin-privacy";
pub const ADMIN_FLAGS_TAG: &str = "admin-feature-flags";
pub const ADMIN_RECSYS_TAG: &str = "admin-recommendations";
pub const ADMIN_OVERVIEW_TAG: &str = "admin-overview";

/// The bearer-JWT `Authorization` header accepted by [`crate::state::AuthUser`].
pub const BEARER_AUTH: &str = "bearer_auth";

/// The single-use `ticket` query parameter accepted by `GET /v1/me/stream`.
///
/// Replaces a raw bearer token in the query string, which no `OpenAPI` security scheme could
/// express; an `apiKey` in `query` can.
pub const STREAM_TICKET_AUTH: &str = "stream_ticket";

struct SecurityAddon;

impl Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        if let Some(components) = openapi.components.as_mut() {
            components.add_security_scheme(
                BEARER_AUTH,
                SecurityScheme::Http(Http::new(HttpAuthScheme::Bearer)),
            );
            components.add_security_scheme(
                STREAM_TICKET_AUTH,
                SecurityScheme::ApiKey(ApiKey::Query(ApiKeyValue::with_description(
                    "ticket",
                    "Single-use, 30-second ticket from `POST /v1/me/stream-ticket`. \
                     `EventSource` cannot set an `Authorization` header, so the credential has \
                     to ride in the URL; making it single-use and short-lived is what stops the \
                     access log, `Referer` header and browser history that record it from being \
                     worth reading (SEC-8).",
                ))),
            );
        }
    }
}

#[derive(OpenApi)]
#[openapi(
    modifiers(&SecurityAddon),
    tags(
        (name = AUTH_TAG, description = "Registration, login, session refresh/logout"),
        (name = SERIES_TAG, description = "Public catalogue browse/detail/chapters"),
        (name = LEGAL_TAG, description = "Operator-published legal documents; unauthenticated"),
        (name = BRANDING_TAG, description = "The deployment's own name, wordmark and copyright; unauthenticated"),
        (name = ME_WATCHLIST_TAG, description = "The signed-in user's watchlist"),
        (name = ME_PROGRESS_TAG, description = "Per-series reading progress"),
        (name = ME_DASHBOARD_TAG, description = "Feed, continue-reading, recommendations, stats"),
        (name = ME_ACCOUNT_TAG, description = "Profile and active-session management"),
        (name = ME_NOTIFICATIONS_TAG, description = "In-app notifications and the live SSE stream"),
        (name = ME_SYNC_TAG, description = "External tracker sync (AniList, ...), user-facing"),
        (name = ADMIN_PROVIDERS_TAG, description = "Operator provider CRUD, state and health"),
        (name = ADMIN_SCANS_TAG, description = "Scan run history, failures, live scan stream"),
        (name = ADMIN_MATCHING_TAG, description = "Series merge-candidate review"),
        (name = ADMIN_CATALOGUE_TAG, description = "Catalogue maintenance: the series list, bulk deletion and the purge"),
        (name = ADMIN_SYNC_TAG, description = "Operator visibility into external sync mappings"),
        (name = ADMIN_USERS_TAG, description = "User directory, identity, suspension and permission grants"),
        (name = ADMIN_PRIVACY_TAG, description = "The GDPR data-subject request queue and its fulfilment"),
        (name = ADMIN_FLAGS_TAG, description = "Runtime feature flags — the deployment control plane"),
        (name = ADMIN_RECSYS_TAG, description = "Recommendation model health, tuning and rebuilds"),
        (name = ADMIN_OVERVIEW_TAG, description = "System stats and audit log"),
    ),
    components(schemas(
    // --- domain entities ---
    tankovault_domain::Provider,
    tankovault_domain::ScanRun,
    tankovault_domain::Tag,
    tankovault_domain::Author,
    tankovault_domain::Politeness,
    tankovault_domain::PolitenessInput,
    // --- domain enums ---
    tankovault_domain::ContentType,
    tankovault_domain::SeriesStatus,
    tankovault_domain::WatchStatus,
    tankovault_domain::RunState,
    tankovault_domain::ScanMode,
    tankovault_domain::AdapterKind,
    tankovault_domain::ProviderState,
    tankovault_domain::TaskState,
    tankovault_domain::AccountStatus,
    // --- authorization and feature registries ---
    tankovault_domain::Permission,
    tankovault_domain::PermissionGroup,
    tankovault_domain::PermissionPreset,
    tankovault_domain::Feature,
    tankovault_domain::FeatureGroup,
    tankovault_domain::Tunable,
    tankovault_domain::TunableGroup,
    tankovault_domain::TunableKind,
    tankovault_domain::Applies,
    // --- domain typed ids ---
    tankovault_domain::SeriesId,
    tankovault_domain::ChapterId,
    tankovault_domain::ProviderId,
    tankovault_domain::ScanRunId,
    tankovault_domain::ScanTaskId,
    tankovault_domain::SeriesSourceId,
    tankovault_domain::TagId,
    tankovault_domain::UserId,
    tankovault_domain::AuthorId,
    tankovault_domain::NotificationId,
    // --- read models served by the admin/series handlers (see `crate::views`) ---
    tankovault_contracts::catalogue::PublicProviderView,
    tankovault_contracts::admin::UserDirectoryRow,
    tankovault_contracts::admin::UserDirectoryPage,
    tankovault_contracts::admin::UserDetailView,
    tankovault_contracts::admin::GrantView,
    tankovault_contracts::me::PrivacyRequestKind,
    tankovault_contracts::me::PrivacyRequestStatus,
    tankovault_contracts::me::PrivacyRequestView,
    tankovault_contracts::admin::AdminPrivacyRequestView,
    tankovault_contracts::admin::FailedTaskView,
    tankovault_contracts::admin::SystemStatsView,
    tankovault_contracts::admin::ProviderStatView,
    tankovault_contracts::admin::AuditView,
    tankovault_contracts::admin::MergeCandidateView,
    tankovault_contracts::admin::SyncAccountView,
    tankovault_contracts::admin::SyncMappingView,
    tankovault_contracts::admin::UnmappedSeriesView,
    tankovault_contracts::admin::RemoteEntryView,
    tankovault_contracts::sync::ConflictView,
    tankovault_contracts::sync::HistoryView,
    tankovault_contracts::me::MeStatsView,
    // --- sync HTTP contract (produced by services/sync, re-published by the /v1/me/sync/* proxies) ---
    tankovault_contracts::sync::AccountStatus,
    tankovault_contracts::sync::AccountSettings,
    tankovault_contracts::sync::ProviderInfo,
    tankovault_contracts::sync::AuthorizeUrl,
    // --- auth ---
    crate::auth::RegisterRequest,
    crate::auth::RegisterResponse,
    crate::auth::LoginRequest,
    crate::auth::TokenResponse,
    crate::auth::ForgotPasswordRequest,
    crate::auth::ResetPasswordRequest,
    crate::auth::VerifyEmailRequest,
    crate::auth::ResendVerificationRequest,
    // --- series ---
    crate::series::SeriesSummary,
    crate::series::SourceDto,
    crate::series::SeriesDetail,
    crate::series::ChapterDto,
    // --- me ---
    crate::me::WatchlistItem,
    crate::me::WatchlistView,
    crate::me::WatchlistCounts,
    crate::me::WatchlistGroup,
    crate::me::WatchlistEntryView,
    crate::me::WatchlistUpsert,
    crate::me::WatchlistBulkUpdate,
    crate::me::WatchlistBulkIds,
    crate::me::BulkResult,
    crate::me::ProgressUpdate,
    crate::me::ProgressDto,
    crate::me::ChapterRead,
    crate::me::MarkReadTo,
    crate::me::SyncExcluded,
    crate::me::MarkRead,
    crate::me::StreamTicket,
    crate::me::FeedEntry,
    crate::me::ContinueItem,
    crate::me::ProfileUpdate,
    crate::me::ProfileDto,
    crate::me::SessionDto,
    crate::me::SyncOpts,
    crate::me::SyncSettingsPatch,
    crate::me::ResolveConflict,
    crate::me::Capabilities,
    crate::me::DeleteAccount,
    crate::me::NewPrivacyRequest,
    // --- admin ---
    crate::admin::CreateProvider,
    crate::admin::UpdateProvider,
    crate::admin::SetProviderState,
    crate::admin::TriggerScan,
    crate::admin::MergeRequest,
    crate::admin::DismissRequest,
    // --- admin: catalogue maintenance ---
    crate::admin::HealthFilter,
    crate::admin::CatalogueRowView,
    crate::admin::CataloguePageView,
    crate::admin::CatalogueSummaryView,
    crate::admin::DeletionView,
    crate::admin::BulkDeleteSeries,
    crate::admin::PurgeScope,
    crate::admin::PurgeRequest,
    crate::admin::PurgeView,
    crate::admin::TestAdapterRequest,
    crate::admin::SyncAccountTarget,
    crate::admin::SyncMappingTarget,
    crate::admin::UpsertMapping,
    crate::admin::SuggestedMatch,
    crate::admin::AssignRemoteEntry,
    // --- admin: user management ---
    crate::admin::UserDetailResponse,
    crate::admin::AdminProfileUpdate,
    crate::admin::SetUserStatus,
    crate::admin::SetPermissions,
    crate::admin::DeleteUser,
    crate::admin::PermissionInfo,
    crate::admin::PresetInfo,
    crate::admin::PermissionCatalogue,
    // --- admin: feature flags ---
    crate::admin::FlagView,
    crate::admin::SetFlag,
    // --- admin: the recommender's control plane ---
    crate::admin::TunableView,
    crate::admin::SetTunable,
    crate::admin::ModelHealthView,
    crate::admin::RebuildRequest,
    tankovault_contracts::admin::RecsysBuildMode,
    tankovault_contracts::admin::RecsysBuildView,
    // --- admin: privacy queue ---
    crate::admin::ResolveRequest,
    crate::admin::ExtendRequest,
    crate::admin::FulfilErasure,
    // --- errors ---
    crate::error::ProblemDetails,
    crate::error::ProblemKind,
)))]
pub struct ApiDoc;
