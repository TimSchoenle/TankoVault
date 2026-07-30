//! The `OpenApi` root. Every handler across [`crate::auth`], [`crate::series`], [`crate::me`]
//! and [`crate::admin`] carries a `#[utoipa::path(..)]` annotation; [`crate::full_openapi`]
//! and [`crate::build_router`] collect them via `utoipa_axum`'s `OpenApiRouter`, and
//! `build_router` serves the resulting spec through a browsable Scalar UI at `/scalar`.
//! `xtask openapi` serialises this document to `openapi.json` and feeds it to `progenitor`,
//! regenerating the typed Rust API client (`crates/api-client`, `src/lib.rs`) that the
//! Dioxus frontend consumes directly (re-exported via `web/frontend/src/wire.rs`).
//!
//! Endpoints that proxy another service's JSON now *return* what they declare (ARCH-10). The
//! six read proxies on `/v1/me/sync/*` are typed as `tankovault_contracts::sync::{ProviderInfo,
//! AuthorizeUrl, AccountStatus, AccountSettings, ConflictView, HistoryView}`, the same
//! definitions `services/sync` produces, so the producing service, this document and the
//! generated client cannot disagree — and [`crate::upstream::Upstream`]'s decode step enforces
//! it at the edge rather than forwarding whatever arrived. They previously returned
//! `Json<serde_json::Value>` while declaring a concrete `body`, which is the drift class
//! `tankovault_contracts::sync` was created to end, reintroduced one layer up. Without the
//! shared definitions the frontend had to hand-mirror those structs, and they drifted.
//!
//! What is still `serde_json::Value` is *declared* as `serde_json::Value`, so the document is
//! honest rather than aspirational. Two groups, neither of which has a fixed schema to publish:
//!
//! - **Command proxies**, whose body is a progress or outcome blob no caller destructures:
//!   `/v1/me/sync/{provider}/{callback,push,pull}`, `DELETE /v1/me/sync/{provider}`,
//!   `/v1/me/sync/conflicts/{id}/resolve` and `/v1/admin/sync/{pull,push,unlink}`.
//! - **Ad-hoc acknowledgements and free-form JSON** produced by this service itself:
//!   `{"ok": true}` / `{"revoked": n}` / `{"removed": b}` on the local admin and
//!   `/v1/me/progress/*` writes, the provider dry-run sample (which is deliberately shaped by
//!   whichever adapter ran), and `/v1/me/notification-prefs`, which is product-defined
//!   free-form JSON, not a fixed schema.
//!
//! The **control-plane scan triggers** — `POST /v1/admin/scans` and
//! `POST /v1/admin/providers/{id}/resolve` — used to be a third such group, and were the one
//! case ARCH-10 could not close from this crate alone: the planner's `{ "run_ids": [...] }` was
//! a private struct in `services/control-plane`'s `main.rs`, so the republisher had nothing
//! more specific to name. That type moved to `tankovault_contracts::admin::ScanTriggeredView`
//! (published as `ScanTriggered`) and both ends name it now, which is why the console's
//! "N scans queued" is finally reading a field a compiler connects to the field the planner
//! writes.
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

use utoipa::openapi::security::{ApiKey, ApiKeyValue, Http, HttpAuthScheme, SecurityScheme};
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
pub const ADMIN_PRIVACY_TAG: &str = "admin-privacy";
pub const ADMIN_FLAGS_TAG: &str = "admin-feature-flags";
pub const ADMIN_OVERVIEW_TAG: &str = "admin-overview";

/// The bearer-JWT `Authorization` header accepted by [`crate::state::AuthUser`].
pub const BEARER_AUTH: &str = "bearer_auth";

/// The single-use `ticket` query parameter accepted by `GET /v1/me/stream`.
///
/// `GET /v1/me/stream` used to be documented as needing no authentication at all — it was listed
/// by name as the one operation whose credential no scheme in this document could express,
/// because a raw bearer token in a query string is not an `OpenAPI` security scheme. Replacing it
/// with a ticket (SEC-8) made it expressible: an `apiKey` in `query` is exactly what this is, so
/// the operation now *declares* its requirement like every other private route, and
/// `tests/openapi_contract.rs` no longer needs an exception for it.
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
        (name = ADMIN_USERS_TAG, description = "User directory, identity, suspension and permission grants"),
        (name = ADMIN_PRIVACY_TAG, description = "The GDPR data-subject request queue and its fulfilment"),
        (name = ADMIN_FLAGS_TAG, description = "Runtime feature flags — the deployment control plane"),
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
    tankovault_domain::AccountStatus,
    // --- authorization and feature registries ---
    tankovault_domain::Permission,
    tankovault_domain::PermissionGroup,
    tankovault_domain::PermissionPreset,
    tankovault_domain::Feature,
    tankovault_domain::FeatureGroup,
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
    // --- sync HTTP contract (produced by services/sync, re-published verbatim by the
    //     `/v1/me/sync/*` proxies here; see `tankovault_contracts::sync`) ---
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
    crate::me::WatchlistUpsert,
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
    // --- admin: privacy queue ---
    crate::admin::ResolveRequest,
    crate::admin::ExtendRequest,
    crate::admin::FulfilErasure,
    // --- errors ---
    crate::error::ProblemDetails,
)))]
pub struct ApiDoc;
