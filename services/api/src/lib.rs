//! Library surface for the `api` service.
//!
//! Everything the binary needs (the route table, shared [`AppState`], and the
//! [`openapi`] schema export that `xtask openapi` serialises and hands to `progenitor`
//! to regenerate the typed API client `crates/api-client` the frontend consumes) lives
//! here; `main.rs` is a thin entrypoint that loads config, wires up infra (DB pool, NATS,
//! audit sink, metrics), and calls [`build_router`].
//!
//! Cross-cutting concerns — rate limiting, security headers, CORS, request ids, metrics,
//! timeouts, body caps, graceful shutdown, health probes — are not implemented here. They
//! come from `tankovault-service`, which every service in the workspace shares.

// Handlers are `pub` so the router (and `openapi::ApiDoc`) can name them, but their
// containing modules stay private — this crate's only real external surface is
// `openapi` and the few items re-exported below, so the lint is noise everywhere else.
#![allow(unreachable_pub)]

pub mod openapi;

mod admin;
mod audit;
mod auth;
mod error;
mod mailer;
mod me;
mod series;
mod state;

use axum::Router;
pub use state::AppState;
use tankovault_config::{RateLimitConfig, SecurityConfig};
use tankovault_service::{Health, HttpStack, MetricsRegistry, RateLimiter, RouteClassifier};
use utoipa::OpenApi as _;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use utoipa_scalar::{Scalar, Servable};

pub fn full_openapi() -> utoipa::openapi::OpenApi {
    documented_router().split_for_parts().1
}

/// Which rate-limit budget each route family draws from.
///
/// Kept next to the route table so adding an endpoint and classifying it are one edit.
/// Anything unlisted falls back to the global budget, which is the safe default: a new
/// route is limited from the moment it exists rather than being unlimited until someone
/// remembers to add it here.
#[must_use]
pub fn route_classifier() -> RouteClassifier {
    RouteClassifier::new()
        // Credential handling — the online-guessing surface.
        .auth("/v1/auth")
        // Cheap to ask for, expensive to serve.
        .expensive("/v1/me/export")
        .expensive("/v1/me/sync")
        .expensive("/v1/admin/scans")
        .expensive("/v1/admin/sync")
        .expensive("/v1/admin/providers/{id}/test")
        .expensive("/v1/admin/series/merge")
}

/// Assemble the full route table and the shared middleware stack.
///
/// The documented endpoints come from [`documented_router`]; `split_for_parts` hands back
/// the plain `axum::Router` alongside the collected `OpenApi` document, which is served
/// (with a browsable UI) via Scalar at `/scalar`.
///
/// The ops probes (`/health`, `/ready`, the metrics scrape) are merged in **outside** the
/// middleware stack: a rate limit or a body cap must never be able to make a healthy
/// replica look unhealthy to its orchestrator.
pub fn build_router(
    state: AppState,
    security: &SecurityConfig,
    rate_limit: &RateLimitConfig,
    metrics: MetricsRegistry,
    health: Health,
    redis: Option<tankovault_service::ratelimit::RedisStoreHandle>,
) -> Router {
    let (router, api) = documented_router().split_for_parts();

    let limiter = RateLimiter::from_config(rate_limit, route_classifier(), redis);

    let app = HttpStack::new(security, metrics.clone())
        .with_rate_limit(limiter)
        .apply(
            router
                .merge(Scalar::with_url("/scalar", api))
                .with_state(state),
        );

    app.merge(tankovault_service::ops_router(health, metrics))
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
        // GDPR data-subject rights: portability (Art. 20) and erasure (Art. 17)
        .routes(routes!(me::export_data))
        .routes(routes!(me::delete_account))
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

#[cfg(test)]
mod tests {
    use super::*;
    use tankovault_service::RouteClass;

    #[test]
    fn credential_routes_draw_from_the_auth_budget() {
        let classifier = route_classifier();
        assert_eq!(classifier.classify("/v1/auth/login"), RouteClass::Auth);
        assert_eq!(classifier.classify("/v1/auth/register"), RouteClass::Auth);
        assert_eq!(
            classifier.classify("/v1/auth/reset-password"),
            RouteClass::Auth
        );
    }

    #[test]
    fn the_data_export_is_expensive_not_ordinary() {
        // `/v1/me/export` sits under `/v1/me`, which is otherwise unclassified; the
        // longest-prefix rule is what keeps it in the tighter budget.
        assert_eq!(
            route_classifier().classify("/v1/me/export"),
            RouteClass::Expensive
        );
    }

    #[test]
    fn ordinary_reads_fall_back_to_the_global_budget() {
        let classifier = route_classifier();
        assert_eq!(classifier.classify("/v1/series"), RouteClass::Global);
        assert_eq!(classifier.classify("/v1/me/watchlist"), RouteClass::Global);
    }
}
