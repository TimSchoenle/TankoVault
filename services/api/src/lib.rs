//! Library surface for the `api` service.
//!
//! Everything the binary needs (the route table, shared [`AppState`], and the
//! [`openapi`] schema export that `xtask openapi` serialises and hands to `progenitor`
//! to regenerate the typed API client `crates/api-client` the frontend consumes) lives
//! here; `main.rs` is a thin entrypoint that loads config, wires up infra (DB pool, NATS,
//! metrics), and calls [`build_router`].

// Handlers are `pub` so the router (and `openapi::ApiDoc`) can name them, but their
// containing modules stay private — this crate's only real external surface is
// `openapi` and the few items re-exported below, so the lint is noise everywhere else.
#![allow(unreachable_pub)]

pub mod openapi;

mod admin;
mod auth;
mod error;
mod mailer;
mod me;
mod series;
mod state;

use axum::Router;
use axum::routing::get;
pub use state::AppState;
use tower_http::compression::CompressionLayer;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use utoipa::OpenApi as _;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use utoipa_scalar::{Scalar, Servable};

pub fn full_openapi() -> utoipa::openapi::OpenApi {
    documented_router().split_for_parts().1
}

/// Assemble the full route table and middleware stack. Kept out of `main` so the router
/// wiring stays readable as endpoints grow (frontend §9 added the reading-dashboard,
/// account, and console-users routes here).
///
/// The documented endpoints come from [`documented_router`]; the undocumented ops probes
/// (`/health`, `/ready`, `/metrics`) are added here. `split_for_parts` then hands back the
/// plain `axum::Router` alongside the collected `OpenApi` document, which is served (with a
/// browsable UI) via Scalar at `/scalar`.
pub fn build_router(state: AppState) -> Router {
    let (router, api) = documented_router()
        // ops (undocumented — no OpenAPI value in a health/metrics probe)
        .route("/health", get(|| async { "ok" }))
        .route("/ready", get(|| async { "ok" }))
        .route("/metrics", get(metrics_handler))
        .split_for_parts();

    router
        .merge(Scalar::with_url("/scalar", api))
        .layer(CompressionLayer::new())
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// The single, authoritative registration of every documented endpoint, shared by
/// [`full_openapi`] (the spec export consumed by `xtask openapi`) and [`build_router`] (the
/// live server) so the two can never drift apart. Each handler carries a
/// `#[utoipa::path(..)]` attribute that `utoipa_axum`'s `routes!` macro reads to build both
/// the `axum` route and its `OpenApi` path entry.
fn documented_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::with_openapi(openapi::ApiDoc::openapi())
        // auth
        .routes(routes!(auth::register))
        .routes(routes!(auth::login))
        .routes(routes!(auth::refresh))
        .routes(routes!(auth::logout))
        .routes(routes!(auth::forgot_password))
        .routes(routes!(auth::reset_password))
        .routes(routes!(auth::verify_email))
        .routes(routes!(auth::resend_verification))
        // public series
        .routes(routes!(series::list))
        .routes(routes!(series::detail))
        .routes(routes!(series::chapters))
        .routes(routes!(series::tags))
        // public provider list for the Discover filter (§9.3)
        .routes(routes!(series::providers))
        // me
        .routes(routes!(me::watchlist))
        .routes(routes!(me::put_watchlist, me::delete_watchlist))
        .routes(routes!(me::get_progress, me::put_progress))
        .routes(routes!(me::put_chapter_progress))
        .routes(routes!(me::mark_read_to))
        .routes(routes!(me::put_sync_excluded))
        .routes(routes!(me::put_sync_override))
        .routes(routes!(me::feed))
        // reading dashboard + recommendations + stats (§9.3)
        .routes(routes!(me::continue_reading))
        .routes(routes!(me::recommendations))
        .routes(routes!(me::stats))
        // account settings (§9.4)
        .routes(routes!(me::patch_profile))
        .routes(routes!(me::sessions))
        .routes(routes!(me::delete_session))
        .routes(routes!(me::notification_prefs, me::put_notification_prefs))
        .routes(routes!(me::notifications))
        .routes(routes!(me::mark_read))
        // live per-user notification stream (SSE; token in query — EventSource cannot set headers)
        .routes(routes!(me::stream))
        // me — external sync, provider-keyed (proxied to the sync service; design: generalized
        // multi-provider sync)
        .routes(routes!(me::sync_providers))
        .routes(routes!(me::sync_authorize_url))
        .routes(routes!(me::sync_status))
        .routes(routes!(me::sync_disconnect))
        .routes(routes!(me::sync_callback))
        .routes(routes!(me::sync_push))
        .routes(routes!(me::sync_pull))
        // me — automatic-sync policy, conflicts and history (design v2 §B.6)
        .routes(routes!(me::sync_settings, me::sync_settings_patch))
        .routes(routes!(me::sync_conflicts))
        .routes(routes!(me::sync_resolve_conflict))
        .routes(routes!(me::sync_history))
        // admin — sync visibility + operator actions (design: admin Sync console tab)
        .routes(routes!(admin::list_sync_accounts))
        .routes(routes!(
            admin::list_sync_mappings,
            admin::upsert_sync_mapping
        ))
        .routes(routes!(admin::list_sync_mappings_for_series))
        .routes(routes!(admin::list_unmapped_series))
        .routes(routes!(admin::list_unmatched_remote))
        .routes(routes!(admin::list_suggestions))
        .routes(routes!(admin::assign_remote_entry))
        .routes(routes!(admin::admin_sync_pull))
        .routes(routes!(admin::admin_sync_push))
        .routes(routes!(admin::admin_sync_unlink))
        .routes(routes!(admin::clear_sync_mapping))
        // admin
        .routes(routes!(admin::system_stats))
        .routes(routes!(admin::audit_log))
        .routes(routes!(admin::list_providers, admin::create_provider))
        .routes(routes!(admin::provider_stats))
        .routes(routes!(admin::update_provider, admin::delete_provider))
        .routes(routes!(admin::set_provider_state))
        .routes(routes!(admin::test_adapter))
        .routes(routes!(admin::resolve_provider))
        .routes(routes!(admin::list_users))
        .routes(routes!(admin::list_scans, admin::trigger_scan))
        .routes(routes!(admin::scan_failures))
        .routes(routes!(admin::scan_stream))
        .routes(routes!(admin::get_scan))
        .routes(routes!(admin::list_merge_candidates))
        .routes(routes!(admin::dismiss_merge_candidate))
        .routes(routes!(admin::merge_series))
}

pub async fn metrics_handler(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> String {
    state.metrics.render()
}

/// Best-effort connection to NATS for the live notification relay. Returns `None` (with a
/// log line) when NATS is unconfigured or unreachable, so `/v1/me/stream` degrades to `503`
/// while the rest of the edge keeps serving.
pub async fn connect_bus(
    nats: Option<&tankovault_config::NatsConfig>,
) -> Option<tankovault_bus::Bus> {
    let Some(nats) = nats else {
        tracing::info!("no NATS configured; /v1/me/stream disabled");
        return None;
    };
    match tankovault_bus::Bus::connect(&nats.url).await {
        Ok(bus) => {
            tracing::info!("connected to NATS for live notification relay");
            Some(bus)
        }
        Err(e) => {
            tracing::warn!(error = %e, "NATS unreachable; /v1/me/stream disabled");
            None
        }
    }
}
