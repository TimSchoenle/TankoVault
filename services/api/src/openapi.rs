//! The wire-schema export consumed by `xtask openapi` to regenerate the frontend's
//! generated types (`web/frontend/src/wire.rs`, via `typify`).
//!
//! This is deliberately **schema-only** — no `#[utoipa::path(..)]` route annotations, so
//! `ApiDoc::openapi().paths` stays empty. The frontend never needs the operation list, only
//! `components.schemas`; annotating all ~70 handlers to also get a browsable Swagger UI is a
//! separate, larger piece of work nobody has asked for yet.
//!
//! Every request/response struct and enum that crosses the wire with a concrete Rust shape
//! is listed below. Endpoints that blindly proxy another service's JSON (`Json<serde_json::
//! Value>` — most of `/v1/me/sync/*` and `/v1/admin/sync/{pull,push,unlink}`) are out of
//! scope: `services/api` itself doesn't know their shape, so there is nothing here to share.
//! `/v1/me/notification-prefs` is excluded for the same reason it's untyped in the handler:
//! it's product-defined free-form JSON, not a fixed schema.
//!
//! Typed ids (`SeriesId`, `UserId`, ...) are listed explicitly below and left with their
//! native `utoipa` "uuid" schema (`{"type":"string","format":"uuid"}`) — `typify` maps that
//! to `uuid::Uuid` on the frontend, so ids are a real, compiler-checked type there too, not
//! a plain `String`.

use utoipa::OpenApi;

#[derive(OpenApi)]
#[openapi(components(schemas(
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
)))]
pub struct ApiDoc;
