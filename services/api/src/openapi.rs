//! The `OpenApi` root. Every handler across [`crate::auth`], [`crate::series`], [`crate::me`]
//! and [`crate::admin`] carries a `#[utoipa::path(..)]` annotation; [`crate::full_openapi`]
//! and [`crate::build_router`] collect them via `utoipa_axum`'s `OpenApiRouter`, and
//! `build_router` serves the resulting spec through a browsable Scalar UI at `/scalar`.
//! `xtask openapi` serialises this document to `openapi.json` and feeds it to `progenitor`,
//! regenerating the typed Rust API client (`crates/api-client`, `src/lib.rs`) that the
//! Dioxus frontend consumes directly (re-exported via `web/frontend/src/wire.rs`).
//!
//! Endpoints that blindly proxy another service's JSON (`Json<serde_json::Value>` — most of
//! `/v1/me/sync/*` and `/v1/admin/sync/{pull,push,unlink}`) document only status codes, no
//! response body schema: `services/api` itself doesn't know their shape, so there is nothing
//! to share. `/v1/me/notification-prefs` is excluded for the same reason it's untyped in the
//! handler: it's product-defined free-form JSON, not a fixed schema.
//!
//! Typed ids (`SeriesId`, `UserId`, ...) are listed explicitly below and left with their
//! native `utoipa` "uuid" schema (`{"type":"string","format":"uuid"}`); `xtask openapi` tags
//! them with `x-rust-type` so `progenitor` maps them back to our domain newtypes on the
//! frontend, keeping ids a real, compiler-checked type there too, not a plain `String`.
//!
//! The `components(schemas(..))` list below is kept even though every listed type is now also
//! reachable transitively from an annotated path (utoipa auto-registers schemas referenced by
//! `request_body`/`responses`): duplicate registration is a no-op, and keeping the explicit
//! list means the generated client can't silently lose a type just because a handler's
//! signature changes.

use utoipa::openapi::security::{Http, HttpAuthScheme, SecurityScheme};
use utoipa::{Modify, OpenApi};

pub const AUTH_TAG: &str = "auth";
pub const SERIES_TAG: &str = "series";
pub const ME_WATCHLIST_TAG: &str = "me-watchlist";
pub const ME_PROGRESS_TAG: &str = "me-progress";
pub const ME_DASHBOARD_TAG: &str = "me-dashboard";
pub const ME_ACCOUNT_TAG: &str = "me-account";
pub const ME_NOTIFICATIONS_TAG: &str = "me-notifications";
pub const ME_SYNC_TAG: &str = "me-sync";
pub const ADMIN_PROVIDERS_TAG: &str = "admin-providers";
pub const ADMIN_SCANS_TAG: &str = "admin-scans";
pub const ADMIN_MATCHING_TAG: &str = "admin-matching";
pub const ADMIN_SYNC_TAG: &str = "admin-sync";
pub const ADMIN_USERS_TAG: &str = "admin-users";
pub const ADMIN_OVERVIEW_TAG: &str = "admin-overview";

/// The bearer-JWT `Authorization` header accepted by [`crate::state::AuthUser`].
pub const BEARER_AUTH: &str = "bearer_auth";

struct SecurityAddon;

impl Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        if let Some(components) = openapi.components.as_mut() {
            components.add_security_scheme(
                BEARER_AUTH,
                SecurityScheme::Http(Http::new(HttpAuthScheme::Bearer)),
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
        (name = ME_WATCHLIST_TAG, description = "The signed-in user's watchlist"),
        (name = ME_PROGRESS_TAG, description = "Per-series reading progress"),
        (name = ME_DASHBOARD_TAG, description = "Feed, continue-reading, recommendations, stats"),
        (name = ME_ACCOUNT_TAG, description = "Profile and active-session management"),
        (name = ME_NOTIFICATIONS_TAG, description = "In-app notifications and the live SSE stream"),
        (name = ME_SYNC_TAG, description = "External tracker sync (AniList, ...), user-facing"),
        (name = ADMIN_PROVIDERS_TAG, description = "Operator provider CRUD, state and health"),
        (name = ADMIN_SCANS_TAG, description = "Scan run history, failures, live scan stream"),
        (name = ADMIN_MATCHING_TAG, description = "Series merge-candidate review"),
        (name = ADMIN_SYNC_TAG, description = "Operator visibility into external sync mappings"),
        (name = ADMIN_USERS_TAG, description = "User administration"),
        (name = ADMIN_OVERVIEW_TAG, description = "System stats and audit log"),
    ),
    components(schemas(
    // --- domain entities ---
    tankovault_domain::Provider,
    tankovault_domain::ScanRun,
    tankovault_domain::Tag,
    tankovault_domain::Author,
    tankovault_domain::Politeness,
    // --- domain enums ---
    tankovault_domain::ContentType,
    tankovault_domain::SeriesStatus,
    tankovault_domain::WatchStatus,
    tankovault_domain::RunState,
    tankovault_domain::ScanMode,
    tankovault_domain::AdapterKind,
    tankovault_domain::ProviderState,
    tankovault_domain::TaskState,
    tankovault_domain::UserRole,
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
    // --- db read models (served directly as JSON by admin/series handlers) ---
    tankovault_db::repo::providers::PublicProvider,
    tankovault_db::repo::users::UserRow2,
    tankovault_db::repo::scans::FailedTaskView,
    tankovault_db::repo::stats::SystemStats,
    tankovault_db::repo::stats::ProviderStat,
    tankovault_db::repo::audit::AuditView,
    tankovault_db::repo::matching::MergeCandidateView,
    tankovault_db::repo::sync::AdminAccountRow,
    tankovault_db::repo::sync::AdminMappingRow,
    tankovault_db::repo::sync::UnmappedSeriesRow,
    tankovault_db::repo::sync::RemoteEntryRow,
    tankovault_db::repo::tracking::MeStats,
    // --- auth ---
    crate::auth::RegisterRequest,
    crate::auth::LoginRequest,
    crate::auth::TokenResponse,
    // --- series ---
    crate::series::SeriesSummary,
    crate::series::SourceDto,
    crate::series::SeriesDetail,
    crate::series::ChapterDto,
    // --- me ---
    crate::me::WatchlistItem,
    crate::me::WatchlistUpsert,
    crate::me::ProgressUpdate,
    crate::me::ProgressDto,
    crate::me::ChapterRead,
    crate::me::MarkReadTo,
    crate::me::SyncExcluded,
    crate::me::MarkRead,
    crate::me::FeedEntry,
    crate::me::ContinueItem,
    crate::me::ProfileUpdate,
    crate::me::ProfileDto,
    crate::me::SessionDto,
    crate::me::SyncOpts,
    crate::me::SyncSettingsPatch,
    crate::me::ResolveConflict,
    // --- admin ---
    crate::admin::CreateProvider,
    crate::admin::UpdateProvider,
    crate::admin::SetProviderState,
    crate::admin::TriggerScan,
    crate::admin::MergeRequest,
    crate::admin::DismissRequest,
    crate::admin::TestAdapterRequest,
    crate::admin::SyncAccountTarget,
    crate::admin::SyncMappingTarget,
    crate::admin::UpsertMapping,
    crate::admin::SuggestedMatch,
    crate::admin::AssignRemoteEntry,
    // --- errors ---
    crate::error::ProblemDetails,
)))]
pub struct ApiDoc;
