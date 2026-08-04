//! Library surface for the `api` service: the route table, shared [`AppState`], and the
//! [`openapi`] schema export. `main.rs` is the thin entrypoint.

// Handlers are `pub` for the router and `openapi::ApiDoc`; containing modules stay private.
#![expect(
    unreachable_pub,
    reason = "a service crate with a thin lib target for its tests; handlers are `pub` for \
              utoipa's generated siblings, not for an external consumer"
)]

pub mod openapi;

mod admin;
mod audit;
mod auth;
mod error;
mod legal;
mod mailer;
mod me;
pub mod passkey;
mod secret;
mod series;
mod slug;
mod state;
pub mod stream_tickets;
mod upstream;
mod views;

use axum::Router;
pub use legal::LegalDocs;
pub use passkey::{RelyingParty, SharedRelyingParty};
pub use state::AppState;
use tankovault_config::{RateLimitConfig, SecurityConfig};
use tankovault_domain::Feature;
use tankovault_service::{
    FeatureGate, FeatureLayer, Health, HttpStack, MetricsRegistry, RateLimiter, RouteClassifier,
    RouteFeatures,
};
pub use upstream::Upstream;
use utoipa::OpenApi as _;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use utoipa_scalar::{Scalar, Servable};

pub fn full_openapi() -> utoipa::openapi::OpenApi {
    documented_router().split_for_parts().1
}

/// Which rate-limit budget each route family draws from.
///
/// Anything unlisted falls back to the global budget: a new route is limited from the
/// moment it exists rather than unlimited until someone classifies it.
#[must_use]
pub fn route_classifier() -> RouteClassifier {
    RouteClassifier::new()
        // Online-guessing surface: `login/start` is unauthenticated and mints a row per call,
        // so leaving passkeys unclassified would let it fill `webauthn_ceremonies` freely.
        .auth("/v1/auth")
        // Verifies an argon2id hash before issuing a challenge — same guessing surface as
        // `/v1/auth`, so a stolen access token must not buy a looser-rate password oracle.
        .auth("/v1/me/passkeys/register/start")
        // Cheap to ask for, expensive to serve — genuinely heavy however they are called.
        .expensive("/v1/me/export")
        .expensive("/v1/me/sync/{provider}/push")
        .expensive("/v1/me/sync/{provider}/pull")
        .expensive("/v1/admin/providers/{id}/test")
        .expensive("/v1/admin/series/merge")
        // A sweep re-blocks the whole catalogue; a key rebuild rewrites every normalized
        // title — the heaviest calls the console can make.
        .expensive("/v1/admin/merge-candidates/sweep")
        .expensive("/v1/admin/matching/rebuild-keys")
        // Only mutating admin calls draw from the tight budget, so read-heavy console pages
        // aren't throttled for merely loading.
        .expensive_write("/v1/admin/scans")
        .expensive_write("/v1/admin/sync")
        // Disclosing another person's whole record, and the erasure that follows an erasure
        // request, are as heavy as the self-service versions already classified above.
        .expensive("/v1/admin/privacy/requests/{id}/export")
        .expensive("/v1/admin/privacy/requests/{id}/fulfil-erasure")
        .expensive("/v1/admin/users/{id}")
}

/// Which [`Feature`] each route family belongs to.
///
/// Deliberately not exhaustive: an unlisted route is ungated. That's right for the substrate
/// — credential endpoints, the capabilities probe, the ops router — since switching those off
/// would be turning the service off, not a feature decision.
#[must_use]
pub fn route_features() -> RouteFeatures {
    // Gates come from the single declaration in `tankovault_contracts::sync`, so the API
    // and sync service cannot drift apart on which routes are gated.
    let sync = tankovault_contracts::sync::sync_route_features()
        .iter()
        .fold(RouteFeatures::new(), |table, (suffix, feature)| {
            table.gate(format!("/v1/me/sync{suffix}"), *feature)
        });

    sync
        // --- public catalogue ---
        .gate("/v1/series", Feature::CatalogueBrowse)
        .gate("/v1/tags", Feature::CatalogueBrowse)
        .gate("/v1/providers", Feature::CatalogueBrowse)
        // --- accounts ---
        .gate("/v1/auth/register", Feature::AccountsRegistration)
        .gate("/v1/auth/password", Feature::AccountsPasswordReset)
        .gate("/v1/auth/verify-email", Feature::AccountsEmailVerification)
        .gate("/v1/me/profile", Feature::AccountsProfile)
        .gate("/v1/me/sessions", Feature::AccountsSessions)
        // Both halves of passkeys share one flag: switching off sign-in without also
        // disabling management would leave a live credential the owner can't revoke.
        .gate("/v1/auth/passkey", Feature::AccountsPasskeys)
        .gate("/v1/me/passkeys", Feature::AccountsPasskeys)
        // --- privacy ---
        .gate("/v1/me/export", Feature::PrivacySelfExport)
        // Exact: `DELETE /v1/me` is self-service erasure, and `/v1/me` is the prefix of the
        // entire signed-in surface. A prefix rule would switch off the whole app.
        .gate_path("/v1/me", Feature::PrivacySelfErasure)
        .gate("/v1/me/privacy", Feature::PrivacyRequests)
        .gate("/v1/admin/privacy", Feature::PrivacyRequests)
        // --- tracking ---
        .gate("/v1/me/watchlist", Feature::TrackingWatchlist)
        // One rule covers the whole progress family, including the bulk mark-read-to write.
        // `feature_gating.rs` checks every gated prefix matches a published route.
        .gate("/v1/me/progress", Feature::TrackingProgress)
        .gate("/v1/me/feed", Feature::TrackingFeed)
        .gate("/v1/me/continue", Feature::TrackingFeed)
        .gate("/v1/me/stats", Feature::TrackingStats)
        .gate("/v1/me/recommendations", Feature::CatalogueRecommendations)
        // --- notifications ---
        .gate("/v1/me/notifications", Feature::NotificationsInApp)
        .gate("/v1/me/stream", Feature::NotificationsLive)
        .gate(
            "/v1/me/notification-prefs",
            Feature::NotificationsPreferences,
        )
        // --- operator surfaces ---
        .gate("/v1/admin/providers", Feature::AdminProviders)
        .gate("/v1/admin/providers/{id}/test", Feature::AdminAdapterTest)
        .gate(
            "/v1/admin/providers/{id}/resolve",
            Feature::AdminAdapterTest,
        )
        // Reads stay reachable when manual scanning is off: an operator who has just disabled
        // scan triggers still needs the history that made them do it.
        .gate_writes("/v1/admin/scans", Feature::ScanningManual)
        .gate("/v1/admin/merge-candidates", Feature::ScanningMergeQueue)
        .gate("/v1/admin/series/merge", Feature::ScanningMergeQueue)
        // Longest prefix overrides the rule above for the sweep — the one route here that
        // deletes series. The control plane re-checks the same flag independently.
        .gate(
            "/v1/admin/merge-candidates/sweep",
            Feature::ScanningAutoMerge,
        )
        .gate("/v1/admin/matching", Feature::ScanningMergeQueue)
        .gate("/v1/admin/sync", Feature::AdminSync)
        .gate("/v1/admin/audit", Feature::AdminAudit)
        .gate("/v1/admin/stats", Feature::AdminStats)
        .gate("/v1/admin/users", Feature::AdminUsers)
        .gate("/v1/admin/permissions", Feature::AdminUsers)
        .gate("/v1/admin/feature-flags", Feature::AdminFeatureFlags)
}

/// Assemble the full route table and the shared middleware stack.
///
/// The ops probes (`/health`, `/ready`, metrics) are merged in **outside** the middleware
/// stack: a rate limit or body cap must never make a healthy replica look unhealthy.
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
    let features = FeatureLayer::new(state.features.clone(), route_features());

    // Feature enforcement runs inside the shared stack: a refused request must still count
    // against the rate limit and get a request id.
    //
    // `/scalar` publishes every admin path and exact request bodies with no auth gate; off
    // by default in production.
    let router = if security.expose_api_docs {
        router.merge(Scalar::with_url("/scalar", api))
    } else {
        router
    };

    let principal = bearer_principal_resolver(state.jwt_secret.clone());

    let app = HttpStack::new(security, metrics.clone())
        .with_rate_limit(limiter)
        .with_principal(Some(principal))
        .apply(
            router
                .with_state(state)
                .layer(axum::middleware::from_fn_with_state(
                    features,
                    tankovault_service::flags::enforce,
                )),
        );

    app.merge(tankovault_service::ops_router(health, metrics))
}

/// Identify the caller from a `Bearer` access token, for per-account rate limiting.
///
/// The signature is verified first: an unverified header would let any caller mint unlimited
/// fresh buckets. Cheaper than [`crate::state::AuthUser`] — no DB round trip or permission
/// check — since this only needs to name a bucket; the handler refuses a suspended account.
fn bearer_principal_resolver(
    jwt_secret: std::sync::Arc<secrecy::SecretSlice<u8>>,
) -> tankovault_service::http::PrincipalResolver {
    std::sync::Arc::new(move |headers: &axum::http::HeaderMap| {
        let token = headers
            .get(axum::http::header::AUTHORIZATION)?
            .to_str()
            .ok()?
            .strip_prefix("Bearer ")?;
        let claims = tankovault_auth::verify_access_token(&jwt_secret, token).ok()?;
        Some(claims.user_id()?.as_uuid().to_string())
    })
}

/// Load the deployment's flag overrides and keep them fresh for the lifetime of the process.
///
/// Awaited before the listener binds, so the first request after a deploy sees the operator's
/// stored decisions rather than briefly reverting to compiled defaults.
pub async fn install_feature_gate(
    pool: tankovault_db::PgPool,
    cfg: &tankovault_config::FeaturesConfig,
    shutdown: tokio_util::sync::CancellationToken,
) -> FeatureGate {
    let gate = FeatureGate::new(std::sync::Arc::new(
        tankovault_service::PostgresFlagSource::new(pool),
    ));
    gate.spawn_refresh(cfg.refresh_interval(), shutdown).await;
    gate
}

/// Registers every documented endpoint, shared by [`full_openapi`] and [`build_router`] so
/// the two can never drift apart.
#[expect(
    clippy::too_many_lines,
    reason = "a route table: one line per endpoint, and splitting it would mean the \
              registration no longer reads as a single list of what this service serves"
)]
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
        // auth — passkey login (two legs: challenge, then signed assertion)
        .routes(routes!(auth::passkey_login_start))
        .routes(routes!(auth::passkey_login_finish))
        // public series
        .routes(routes!(series::list))
        .routes(routes!(series::detail))
        .routes(routes!(series::chapters))
        .routes(routes!(series::tags))
        // public provider list for the Discover filter (§9.3)
        .routes(routes!(series::providers))
        // Legal documents, deliberately unauthenticated: registering is the act of accepting
        // the Terms, so the register form has to be able to link them to a signed-out reader.
        .routes(routes!(legal::legal_index))
        .routes(routes!(legal::legal_document))
        // me
        .routes(routes!(me::watchlist))
        .routes(routes!(me::watchlist_summary))
        // Bulk before the `{series_id}` sibling for readability only — `matchit` prefers the
        // static segment regardless, and a `series_id` is a uuid, so `bulk` can never be one.
        .routes(routes!(
            me::bulk_update_watchlist,
            me::bulk_remove_watchlist
        ))
        .routes(routes!(
            me::get_watchlist_entry,
            me::put_watchlist,
            me::delete_watchlist
        ))
        .routes(routes!(me::get_progress, me::put_progress))
        .routes(routes!(me::put_chapter_progress))
        .routes(routes!(me::mark_read_to))
        .routes(routes!(me::bulk_mark_read))
        .routes(routes!(me::put_sync_excluded))
        .routes(routes!(me::put_sync_override))
        .routes(routes!(me::feed))
        // reading dashboard + recommendations + stats (§9.3)
        .routes(routes!(me::continue_reading))
        .routes(routes!(me::recommendations))
        .routes(routes!(me::stats))
        // capability probe the client gates its whole UI on
        .routes(routes!(me::capabilities))
        // account settings (§9.4)
        .routes(routes!(me::patch_profile))
        .routes(routes!(me::change_password))
        .routes(routes!(me::sessions))
        .routes(routes!(me::delete_session))
        // account settings — passkeys the caller has registered, and the ceremony that adds one
        .routes(routes!(me::list_passkeys))
        .routes(routes!(me::passkey_register_start))
        .routes(routes!(me::passkey_register_finish))
        .routes(routes!(me::rename_passkey, me::delete_passkey))
        .routes(routes!(me::notification_prefs, me::put_notification_prefs))
        .routes(routes!(me::notifications))
        .routes(routes!(me::mark_read))
        // GDPR data-subject rights: portability (Art. 20) and erasure (Art. 17), plus the
        // tracked request queue that covers the rights those two cannot serve directly
        .routes(routes!(me::export_data))
        .routes(routes!(me::delete_account))
        .routes(routes!(
            me::list_privacy_requests,
            me::create_privacy_request
        ))
        .routes(routes!(me::cancel_privacy_request))
        // live SSE stream + the mint for its query credential (a single-use ticket, not the
        // access token). Both gated by the one `/v1/me/stream` prefix rule below.
        .routes(routes!(me::stream, me::stream_ticket))
        // me — external sync, provider-keyed (proxied to the sync service)
        .routes(routes!(me::sync_providers))
        .routes(routes!(me::sync_authorize_url))
        .routes(routes!(me::sync_status))
        .routes(routes!(me::sync_disconnect))
        .routes(routes!(me::sync_callback))
        .routes(routes!(me::sync_push))
        .routes(routes!(me::sync_pull))
        // me — automatic-sync policy, conflicts and history
        .routes(routes!(me::sync_settings, me::sync_settings_patch))
        .routes(routes!(me::sync_conflicts))
        .routes(routes!(me::sync_resolve_conflict))
        .routes(routes!(me::sync_history))
        // admin — sync visibility + operator actions
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
        // admin — user administration: directory, identity, suspension, sessions, grants,
        // erasure, and the catalogue of what can be granted
        .routes(routes!(admin::list_users))
        .routes(routes!(
            admin::get_user,
            admin::update_user,
            admin::delete_user
        ))
        .routes(routes!(admin::set_user_status))
        .routes(routes!(admin::set_user_permissions))
        .routes(routes!(admin::revoke_user_sessions))
        .routes(routes!(admin::verify_user_email))
        .routes(routes!(admin::permission_catalogue))
        // admin — the deployment control plane: every feature and its switch
        .routes(routes!(admin::list_flags))
        .routes(routes!(admin::set_flag, admin::reset_flag))
        // admin — the GDPR data-subject request queue and its fulfilment
        .routes(routes!(admin::list_privacy_queue))
        .routes(routes!(admin::claim_privacy_request))
        .routes(routes!(admin::resolve_privacy_request))
        .routes(routes!(admin::extend_privacy_request))
        .routes(routes!(admin::export_subject_data))
        .routes(routes!(admin::fulfil_erasure))
        .routes(routes!(admin::list_scans, admin::trigger_scan))
        .routes(routes!(admin::scan_failures))
        .routes(routes!(admin::scan_stream))
        .routes(routes!(admin::get_scan))
        .routes(routes!(admin::list_merge_candidates))
        .routes(routes!(admin::dismiss_merge_candidate))
        .routes(routes!(admin::merge_series))
        .routes(routes!(admin::sweep_merge_candidates))
        .routes(routes!(admin::rebuild_matching_keys))
}

/// Best-effort connection to NATS for the live notification relay. Returns `None` when
/// unconfigured or unreachable, so `/v1/me/stream` degrades to `503` while the edge keeps serving.
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
    use axum::http::Method;
    use tankovault_service::RouteClass;

    #[test]
    fn credential_routes_draw_from_the_auth_budget() {
        let classifier = route_classifier();
        assert_eq!(
            classifier.classify(&Method::POST, "/v1/auth/login"),
            RouteClass::Auth
        );
        assert_eq!(
            classifier.classify(&Method::POST, "/v1/auth/register"),
            RouteClass::Auth
        );
        assert_eq!(
            classifier.classify(&Method::POST, "/v1/auth/password/reset"),
            RouteClass::Auth
        );
    }

    #[test]
    fn the_data_export_is_expensive_not_ordinary() {
        // `/v1/me` is otherwise unclassified; longest-prefix is what keeps this in budget.
        assert_eq!(
            route_classifier().classify(&Method::GET, "/v1/me/export"),
            RouteClass::Expensive
        );
    }

    #[test]
    fn disclosing_or_erasing_another_person_is_expensive() {
        let classifier = route_classifier();
        assert_eq!(
            classifier.classify(&Method::GET, "/v1/admin/privacy/requests/{id}/export"),
            RouteClass::Expensive
        );
        assert_eq!(
            classifier.classify(&Method::DELETE, "/v1/admin/users/{id}"),
            RouteClass::Expensive
        );
    }

    #[test]
    fn ordinary_reads_fall_back_to_the_global_budget() {
        let classifier = route_classifier();
        assert_eq!(
            classifier.classify(&Method::GET, "/v1/series"),
            RouteClass::Global
        );
        assert_eq!(
            classifier.classify(&Method::GET, "/v1/me/watchlist"),
            RouteClass::Global
        );
    }

    #[test]
    fn the_substrate_is_never_behind_a_flag() {
        // Turning off sign-in, the capabilities probe or ops is turning the service off,
        // not a product decision — these must have no rule at all.
        let features = route_features();
        for path in [
            "/v1/auth/login",
            "/v1/auth/refresh",
            "/v1/auth/logout",
            "/v1/me/capabilities",
            "/health",
            "/ready",
        ] {
            assert_eq!(
                features.required(&Method::POST, path),
                None,
                "{path} must not be gated"
            );
        }
    }

    #[test]
    fn the_scan_history_survives_disabling_manual_scans() {
        let features = route_features();
        assert_eq!(features.required(&Method::GET, "/v1/admin/scans"), None);
        assert_eq!(
            features.required(&Method::POST, "/v1/admin/scans"),
            Some(Feature::ScanningManual)
        );
    }

    #[test]
    fn the_finer_sync_routes_beat_the_broad_one() {
        let features = route_features();
        assert_eq!(
            features.required(&Method::GET, "/v1/me/sync/providers"),
            Some(Feature::SyncExternal)
        );
        assert_eq!(
            features.required(&Method::GET, "/v1/me/sync/conflicts"),
            Some(Feature::SyncConflictReview)
        );
        assert_eq!(
            features.required(&Method::GET, "/v1/me/sync/history"),
            Some(Feature::SyncHistory)
        );
    }

    /// Every suffix in the shared declaration is actually gated under this tier's prefix.
    ///
    /// The two tiers used to keep independent tables and had drifted — gated `/conflicts` and
    /// `/history` but not `/push-series` — with nothing asserting they agreed. `services/sync`
    /// carries the mirror of this test.
    #[test]
    fn the_shared_sync_declaration_is_applied_under_this_tier_s_prefix() {
        let features = route_features();
        for (suffix, expected) in tankovault_contracts::sync::sync_route_features() {
            let path = format!("/v1/me/sync{suffix}");
            assert_eq!(
                features.required(&Method::GET, &path),
                Some(*expected),
                "{path} is not gated by the feature the shared declaration names"
            );
        }
    }

    #[test]
    fn the_recovery_paths_are_gated_only_by_features_that_cannot_be_switched_off() {
        // Both surfaces *are* declared, so the table is complete — but their features are
        // locked, so declaring them cannot lock anybody out.
        let features = route_features();
        for (path, expected) in [
            ("/v1/admin/users", Feature::AdminUsers),
            ("/v1/admin/feature-flags", Feature::AdminFeatureFlags),
        ] {
            assert_eq!(features.required(&Method::GET, path), Some(expected));
            assert!(expected.is_locked(), "{expected} must be locked");
        }
    }

    #[test]
    fn every_route_backed_feature_is_actually_reachable() {
        // A feature nobody gates is a switch that does nothing. These are enforced elsewhere
        // — background loops, outbound channels, behaviour decided inside a handler.
        let declared = route_features().declared_features();
        let enforced_elsewhere = [
            Feature::CatalogueSearch,
            Feature::NotificationsEmail,
            Feature::NotificationsWebhook,
            Feature::NotificationsDiscord,
            Feature::SyncAutoPush,
            Feature::SyncScheduledPull,
            Feature::ScanningScheduler,
            Feature::ScanningFull,
        ];
        for feature in Feature::all() {
            assert!(
                declared.contains(feature) || enforced_elsewhere.contains(feature),
                "{feature} is switchable but nothing enforces it"
            );
        }
    }
}
