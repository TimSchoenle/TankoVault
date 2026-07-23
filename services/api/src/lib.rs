//! Library surface for the `api` service.
//!
//! Everything the binary needs (the route table, shared [`AppState`], and the
//! [`openapi`] schema export used by `xtask openapi` to regenerate the frontend's
//! generated wire types) lives here; `main.rs` is a thin entrypoint that loads
//! config, wires up infra (DB pool, NATS, metrics), and calls [`build_router`].

// Handlers are `pub` so the router (and `openapi::ApiDoc`) can name them, but their
// containing modules stay private — this crate's only real external surface is
// `openapi` and the few items re-exported below, so the lint is noise everywhere else.
#![allow(unreachable_pub)]

pub mod openapi;

mod admin;
mod auth;
mod error;
mod me;
mod series;
mod state;

use axum::Router;
use axum::routing::{get, patch, post, put};
pub use state::AppState;
use tower_http::compression::CompressionLayer;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

/// Assemble the full route table and middleware stack. Kept out of `main` so the router
/// wiring stays readable as endpoints grow (frontend §9 added the reading-dashboard,
/// account, and console-users routes here).
pub fn build_router(state: AppState) -> Router {
    Router::new()
        // auth
        .route("/v1/auth/register", post(auth::register))
        .route("/v1/auth/login", post(auth::login))
        .route("/v1/auth/refresh", post(auth::refresh))
        .route("/v1/auth/logout", post(auth::logout))
        // public series
        .route("/v1/series", get(series::list))
        .route("/v1/series/{id}", get(series::detail))
        .route("/v1/series/{id}/chapters", get(series::chapters))
        .route("/v1/tags", get(series::tags))
        // public provider list for the Discover filter (§9.3)
        .route("/v1/providers", get(series::providers))
        // me
        .route("/v1/me/watchlist", get(me::watchlist))
        .route(
            "/v1/me/watchlist/{series_id}",
            put(me::put_watchlist).delete(me::delete_watchlist),
        )
        .route(
            "/v1/me/progress/{series_id}",
            get(me::get_progress).put(me::put_progress),
        )
        .route(
            "/v1/me/progress/{series_id}/chapters/{number}",
            put(me::put_chapter_progress),
        )
        .route(
            "/v1/me/progress/{series_id}/mark-read-to",
            post(me::mark_read_to),
        )
        .route(
            "/v1/me/watchlist/{series_id}/sync",
            put(me::put_sync_excluded),
        )
        .route(
            "/v1/me/watchlist/{series_id}/sync/{provider}",
            put(me::put_sync_override),
        )
        .route("/v1/me/feed", get(me::feed))
        // reading dashboard + recommendations + stats (§9.3)
        .route("/v1/me/continue", get(me::continue_reading))
        .route("/v1/me/recommendations", get(me::recommendations))
        .route("/v1/me/stats", get(me::stats))
        // account settings (§9.4)
        .route("/v1/me/profile", patch(me::patch_profile))
        .route("/v1/me/sessions", get(me::sessions))
        .route(
            "/v1/me/sessions/{id}",
            axum::routing::delete(me::delete_session),
        )
        .route(
            "/v1/me/notification-prefs",
            get(me::notification_prefs).put(me::put_notification_prefs),
        )
        .route("/v1/me/notifications", get(me::notifications))
        .route("/v1/me/notifications/read", post(me::mark_read))
        // live per-user notification stream (SSE; token in query — EventSource cannot set headers)
        .route("/v1/me/stream", get(me::stream))
        // me — external sync, provider-keyed (proxied to the sync service; design: generalized
        // multi-provider sync)
        .route("/v1/me/sync/providers", get(me::sync_providers))
        .route(
            "/v1/me/sync/{provider}/authorize",
            get(me::sync_authorize_url),
        )
        .route("/v1/me/sync/{provider}/status", get(me::sync_status))
        .route(
            "/v1/me/sync/{provider}",
            axum::routing::delete(me::sync_disconnect),
        )
        .route("/v1/me/sync/{provider}/callback", get(me::sync_callback))
        .route("/v1/me/sync/{provider}/push", post(me::sync_push))
        .route("/v1/me/sync/{provider}/pull", post(me::sync_pull))
        // me — automatic-sync policy, conflicts and history (design v2 §B.6)
        .route(
            "/v1/me/sync/{provider}/settings",
            get(me::sync_settings).patch(me::sync_settings_patch),
        )
        .route("/v1/me/sync/conflicts", get(me::sync_conflicts))
        .route(
            "/v1/me/sync/conflicts/{id}/resolve",
            post(me::sync_resolve_conflict),
        )
        .route("/v1/me/sync/history", get(me::sync_history))
        // admin — sync visibility + operator actions (design: admin Sync console tab)
        .route("/v1/admin/sync/accounts", get(admin::list_sync_accounts))
        .route(
            "/v1/admin/sync/mappings",
            get(admin::list_sync_mappings).post(admin::upsert_sync_mapping),
        )
        .route(
            "/v1/admin/sync/series/{id}",
            get(admin::list_sync_mappings_for_series),
        )
        .route("/v1/admin/sync/unmapped", get(admin::list_unmapped_series))
        .route(
            "/v1/admin/sync/unmatched",
            get(admin::list_unmatched_remote),
        )
        .route("/v1/admin/sync/suggest", get(admin::list_suggestions))
        .route("/v1/admin/sync/assign", post(admin::assign_remote_entry))
        .route("/v1/admin/sync/pull", post(admin::admin_sync_pull))
        .route("/v1/admin/sync/push", post(admin::admin_sync_push))
        .route("/v1/admin/sync/unlink", post(admin::admin_sync_unlink))
        .route(
            "/v1/admin/sync/mappings/clear",
            post(admin::clear_sync_mapping),
        )
        // admin
        .route("/v1/admin/stats", get(admin::system_stats))
        .route("/v1/admin/audit", get(admin::audit_log))
        .route(
            "/v1/admin/providers",
            get(admin::list_providers).post(admin::create_provider),
        )
        .route("/v1/admin/providers/stats", get(admin::provider_stats))
        .route(
            "/v1/admin/providers/{id}",
            patch(admin::update_provider).delete(admin::delete_provider),
        )
        .route(
            "/v1/admin/providers/{id}/state",
            post(admin::set_provider_state),
        )
        .route("/v1/admin/providers/{id}/test", post(admin::test_adapter))
        .route(
            "/v1/admin/providers/{id}/resolve",
            post(admin::resolve_provider),
        )
        .route("/v1/admin/users", get(admin::list_users))
        .route(
            "/v1/admin/scans",
            get(admin::list_scans).post(admin::trigger_scan),
        )
        .route("/v1/admin/scan-failures", get(admin::scan_failures))
        .route("/v1/admin/scans/stream", get(admin::scan_stream))
        .route("/v1/admin/scans/{run_id}", get(admin::get_scan))
        .route(
            "/v1/admin/merge-candidates",
            get(admin::list_merge_candidates),
        )
        .route(
            "/v1/admin/merge-candidates/dismiss",
            post(admin::dismiss_merge_candidate),
        )
        .route("/v1/admin/series/merge", post(admin::merge_series))
        // ops
        .route("/health", get(|| async { "ok" }))
        .route("/ready", get(|| async { "ok" }))
        .route("/metrics", get(metrics_handler))
        .layer(CompressionLayer::new())
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
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
