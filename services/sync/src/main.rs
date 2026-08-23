//! External-sync microservice: owns each provider's `OAuth2` flow, encrypts tokens at rest, and
//! reconciles a user's remote list with the local watchlist/progress. The user-facing
//! `/v1/me/sync/{provider}/*` routes on the API delegate to the internal contract exposed here.

mod engine;
mod error;
mod mapping;
mod provider;
mod providers;
mod views;

/// The route patterns this service serves, named once so the router and the authorisation
/// table below cannot spell the same endpoint differently.
mod path {
    pub(crate) const PROVIDERS: &str = "/v1/sync/providers";
    pub(crate) const PUSH_SERIES: &str = "/v1/sync/push-series";
    pub(crate) const ENRICH: &str = "/v1/sync/enrich";
    pub(crate) const AUTHORIZE_URL: &str = "/v1/sync/{provider}/authorize-url";
    pub(crate) const STATUS: &str = "/v1/sync/{provider}/status/{user_id}";
    pub(crate) const LINK: &str = "/v1/sync/{provider}/link";
    pub(crate) const PULL: &str = "/v1/sync/{provider}/pull";
    pub(crate) const PUSH: &str = "/v1/sync/{provider}/push";
    pub(crate) const SETTINGS: &str = "/v1/sync/{provider}/settings/{user_id}";
    pub(crate) const CONFLICTS: &str = "/v1/sync/conflicts/{user_id}";
    pub(crate) const RESOLVE_CONFLICT: &str = "/v1/sync/conflicts/{id}/resolve";
    pub(crate) const HISTORY: &str = "/v1/sync/history/{user_id}";
    pub(crate) const DECISIONS: &str = "/v1/sync/decisions";
    pub(crate) const REVERT_DECISION: &str = "/v1/sync/decisions/{id}/revert";
    pub(crate) const FLAG_DECISION: &str = "/v1/sync/decisions/{id}/flag";
    pub(crate) const MATCH_BLOCKS: &str = "/v1/sync/match-blocks";
}

/// Who may reach each of this service's routes.
///
/// `api` alone, everywhere. Every route here acts on behalf of a named user — reading a
/// reader's progress history, unlinking their tracker account, pulling their list — and `api` is
/// the only tier that authenticates users, so it is the only one with a user to act for. Under
/// one tier-wide token `worker`, `render` and `challenge-solver` all held a credential that
/// opened every one of these.
///
/// A method appears once per verb: `link` and `settings` serve two each, and a table keyed on
/// the path alone would authorise the write because the read was permitted.
static INTERNAL_ROUTES: &[tankovault_service::InternalRoute] = {
    use axum::http::Method;
    use tankovault_service::InternalRoute;

    /// Every route on this service has the same permitted caller; this keeps that visible in
    /// one place rather than repeated eighteen times.
    const API: &[&str] = &["api"];
    const fn route(method: Method, path: &'static str) -> InternalRoute {
        InternalRoute {
            method,
            path,
            callers: API,
        }
    }

    &[
        route(Method::GET, path::PROVIDERS),
        route(Method::POST, path::PUSH_SERIES),
        route(Method::POST, path::ENRICH),
        route(Method::GET, path::AUTHORIZE_URL),
        route(Method::GET, path::STATUS),
        route(Method::POST, path::LINK),
        route(Method::DELETE, path::LINK),
        route(Method::POST, path::PULL),
        route(Method::POST, path::PUSH),
        route(Method::GET, path::SETTINGS),
        route(Method::PATCH, path::SETTINGS),
        route(Method::GET, path::CONFLICTS),
        route(Method::POST, path::RESOLVE_CONFLICT),
        route(Method::GET, path::HISTORY),
        route(Method::GET, path::DECISIONS),
        route(Method::POST, path::REVERT_DECISION),
        route(Method::POST, path::FLAG_DECISION),
        route(Method::POST, path::MATCH_BLOCKS),
    ]
};
/// Merge-engine reconciliation tests; needs Docker, hence the feature gate.
#[cfg(all(test, feature = "integration"))]
mod reconcile_tests;

use tankovault_sync::config::{AniListConfig, Config, MetadataConfig};

use crate::error::AppError;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use secrecy::{ExposeSecret as _, SecretString};
use serde::Deserialize;
use tankovault_service::health::PostgresCheck;
use tankovault_service::{
    CancellationToken, FeatureGate, FeatureLayer, Health, HttpStack, MetricsRegistry,
    PostgresFlagSource, RateLimiter, RouteClassifier, RouteFeatures,
};

use engine::SyncEngine;
use mapping::ConflictPolicy;
use provider::ExternalProvider;
use providers::anilist::AniListClient;
use tankovault_auth::Sealer;
use tankovault_contracts::sync::{
    AccountSettings, AccountStatus, AuthorizeUrl, ConflictView, Flagged, HistoryView, ProviderInfo,
    Removed, Resolved,
};
use tankovault_domain::{Feature, SeriesId, UserId};

// `Clone`: see [`MetadataConfig`]. The `SecretString` fields clone their `Arc`, not the secret.

/// Build the provider registry; register additional providers here as they land.
fn build_providers(
    cfg: AniListConfig,
) -> anyhow::Result<HashMap<&'static str, Box<dyn ExternalProvider>>> {
    let mut providers: HashMap<&'static str, Box<dyn ExternalProvider>> = HashMap::new();
    let anilist = AniListClient::new(
        cfg.graphql_url,
        cfg.oauth_base,
        cfg.client_id,
        cfg.client_secret,
        cfg.redirect_uri,
        Duration::from_millis(cfg.min_request_interval_ms),
    )?;
    providers.insert(crate::providers::anilist::PROVIDER, Box::new(anilist));
    Ok(providers)
}

/// Tokenless metadata-enrichment worker: a periodic sweep filling in description/cover/titles
/// for every series from `AniList`'s public API, no stored user token required.
///
/// Not feature-gated: it belongs to the catalogue, not external sync, so `sync.external` off
/// should not stop descriptions being filled in.
fn spawn_enrichment_worker(
    engine: &Arc<SyncEngine>,
    metadata: &MetadataConfig,
    shutdown: tokio_util::sync::CancellationToken,
) {
    if !metadata.enrich_enabled {
        return;
    }
    let worker = engine.clone();
    let interval = Duration::from_secs(metadata.enrich_interval_secs.max(1));
    let batch = metadata.enrich_batch.max(1);
    let max = metadata.enrich_max_series;
    tokio::spawn(async move {
        tankovault_service::shutdown::every(interval, shutdown, "metadata-enrichment", move || {
            let worker = worker.clone();
            async move {
                if let Err(e) = worker.enrich_all(batch, max).await {
                    tracing::warn!(error = %e, "tokenless metadata enrichment sweep failed");
                }
            }
        })
        .await;
    });
    tracing::info!(
        interval_secs = metadata.enrich_interval_secs,
        "tokenless metadata enrichment worker started"
    );
}

/// Scheduled reconciliation loop: pulls remote-side changes back automatically, closing the
/// reactive-push-only gap. Disabled when `interval_secs` is 0.
fn spawn_reconciliation_loop(
    engine: &Arc<SyncEngine>,
    interval_secs: u64,
    features: &FeatureGate,
    shutdown: tokio_util::sync::CancellationToken,
) {
    if interval_secs == 0 {
        return;
    }
    let engine = engine.clone();
    let features = features.clone();
    let interval = Duration::from_secs(interval_secs);
    // `every` skips its first tick, so boot is not a thundering herd against providers.
    tokio::spawn(async move {
        tankovault_service::shutdown::every(interval, shutdown, "reconcile", move || {
            let engine = engine.clone();
            let features = features.clone();
            async move {
                // Checked per tick, not at boot, so an operator stopping this takes effect
                // immediately and resumes on its own when the flag returns.
                if !features.is_enabled(Feature::SyncExternal)
                    || !features.is_enabled(Feature::SyncScheduledPull)
                {
                    tracing::debug!("skipping reconciliation; scheduled sync is switched off");
                    return;
                }
                engine.reconcile_all_accounts().await;
            }
        })
        .await;
    });
}

#[derive(Clone)]
struct AppState {
    engine: Arc<SyncEngine>,
    /// Enrichment page size for the on-demand `/v1/sync/enrich` route.
    enrich_batch: i64,
    /// Enrichment per-run cap for the on-demand `/v1/sync/enrich` route.
    enrich_max: usize,
}

/// This service's feature-gate table, built from the single suffix-keyed declaration in
/// `tankovault_contracts::sync` — the API gates the same surface from the same list, so the two
/// tiers can't drift apart the way they once did.
fn route_features() -> RouteFeatures {
    tankovault_contracts::sync::sync_route_features()
        .iter()
        .fold(RouteFeatures::new(), |table, (suffix, feature)| {
            table.gate(format!("/v1/sync{suffix}"), *feature)
        })
}

/// Whether `encoded` is a key made entirely of zero bytes. Compares decoded bytes, not the
/// string, so every base64 spelling of 32 zero bytes is caught, not just the compose file's literal.
fn is_placeholder_key(encoded: &SecretString) -> bool {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(encoded.expose_secret().trim())
        .is_ok_and(|bytes| !bytes.is_empty() && bytes.iter().all(|b| *b == 0))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // First, before anything can build a rustls configuration: rustls cannot choose a provider
    // for itself in this graph and panics instead of erroring. See `tankovault_service::crypto`.
    tankovault_service::install_crypto_provider();

    // Before anything else: `scratch` images have no shell/wget, so a Docker HEALTHCHECK
    // invokes this binary itself as the only available probe.
    if tankovault_service::healthcheck::requested() {
        let cfg: Config = tankovault_config::load()?;
        tankovault_service::run_healthcheck_and_exit(&cfg.bind_addr);
    }

    let boot = tankovault_config::load_watched::<Config>()?;
    // Both are process-global and installed once, which is why `telemetry.*` and `metrics.*`
    // are the two blocks a configuration reload cannot apply.
    // Bound, not discarded: the guard flushes queued Sentry events on the way out, and
    // dropping it here would close the client before the service serves anything.
    let _telemetry = tankovault_service::init_tracing(&boot.value.telemetry)?;
    let metrics =
        MetricsRegistry::install(&boot.value.metrics, &boot.value.telemetry.service_name)?;
    let shutdown = tankovault_service::install_shutdown();
    // Outside the reloadable runtime so a reload does not rebind the scrape listener.
    tankovault_service::spawn_metrics_server(metrics.clone(), shutdown.clone());

    tankovault_service::run_reloading(boot, &shutdown, |cfg, generation| {
        serve_once(cfg, metrics.clone(), generation)
    })
    .await
}

/// Build and run everything a configuration change rebuilds: the pool, the token sealer, the
/// provider clients, the background loops, the router and the listener.
///
/// Returns when `shutdown` is cancelled — by the OS signal, or by the supervisor because the
/// configuration changed and this runtime is being replaced. A rotated `AniList` client secret
/// takes effect here; rotating `anilist.token_encryption_key` does not re-seal what is already
/// stored, so that one still needs the migration it always did.
async fn serve_once(
    cfg: Arc<Config>,
    metrics: MetricsRegistry,
    shutdown: CancellationToken,
) -> anyhow::Result<()> {
    // Resolved before anything binds: production refuses to boot rather than silently serving
    // privileged routes without the identity that guards them.
    let internal_auth = &tankovault_service::internal_auth::resolve(&cfg.internal)?;

    let pool = tankovault_db::connect(
        &cfg.database.url,
        cfg.database.max_connections,
        cfg.database.acquire_timeout_secs,
    )
    .await?;
    tankovault_service::metrics::spawn_pool_sampler(pool.clone(), shutdown.clone());
    // The engine takes ownership of the pool; readiness and the feature gate need their own.
    let health_pool = pool.clone();
    let flags_pool = pool.clone();

    // Refused in every profile: this key seals every user's AniList tokens at rest, and the
    // published compose-file fallback is 32 zero bytes — a well-known value anyone can read.
    if is_placeholder_key(&cfg.anilist.token_encryption_key) {
        anyhow::bail!(
            "refusing to start: anilist.token_encryption_key is the all-zero placeholder \
             published in this repository. Every stored OAuth token sealed with it is \
             readable by anyone with a copy of the database. Generate one with \
             `openssl rand -base64 32` and set TANKOVAULT_ANILIST__TOKEN_ENCRYPTION_KEY."
        );
    }
    let secret = Sealer::from_base64_key(&cfg.anilist.token_encryption_key)
        .map_err(|e| anyhow::anyhow!("invalid anilist.token_encryption_key: {e}"))?;
    let default_policy = cfg.anilist.default_conflict_policy;
    let metadata = cfg.metadata.clone();
    let providers = build_providers(cfg.anilist.clone())?;

    let engine = Arc::new(SyncEngine::new(
        pool,
        secret,
        default_policy,
        metadata.priority.clone(),
        metadata.tags.blocklist(),
        &cfg.matching,
        providers,
    ));

    spawn_enrichment_worker(&engine, &metadata, shutdown.clone());

    let state = AppState {
        engine: engine.clone(),
        enrich_batch: metadata.enrich_batch.max(1),
        enrich_max: metadata.enrich_max_series,
    };

    // The operator's runtime switches, loaded before anything starts consulting them.
    let features = FeatureGate::new(Arc::new(PostgresFlagSource::new(flags_pool)));
    features
        .spawn_refresh(cfg.features.refresh_interval(), shutdown.clone())
        .await;

    spawn_reconciliation_loop(
        &engine,
        cfg.reconcile_interval_secs,
        &features,
        shutdown.clone(),
    );

    let routes = Router::new()
        .route(path::PROVIDERS, get(providers_list))
        .route(path::PUSH_SERIES, post(push_series))
        .route(path::ENRICH, post(enrich))
        .route(path::AUTHORIZE_URL, get(authorize_url))
        .route(path::STATUS, get(status))
        .route(path::LINK, post(link).delete(unlink))
        .route(path::PULL, post(pull))
        .route(path::PUSH, post(push))
        .route(path::SETTINGS, get(get_settings).patch(patch_settings))
        .route(path::CONFLICTS, get(list_conflicts))
        .route(path::RESOLVE_CONFLICT, post(resolve_conflict))
        .route(path::HISTORY, get(list_history))
        .route(path::DECISIONS, get(list_decisions))
        .route(path::REVERT_DECISION, post(revert_decision))
        .route(path::FLAG_DECISION, post(flag_decision))
        .route(path::MATCH_BLOCKS, post(block_match))
        .with_state(state)
        // Same declarative gate the public API uses: this service is reachable from anywhere on
        // the internal network, so the switch must hold here too, not just at the edge.
        .layer(axum::middleware::from_fn_with_state(
            FeatureLayer::new(features.clone(), route_features()),
            tankovault_service::flags::enforce,
        ));

    // Postgres is the only hard dependency: a provider outage is expected and already
    // degrades per-request, so probing AniList here would flap readiness on their uptime.
    let health = Health::builder()
        .check(PostgresCheck::new(health_pool))
        .build();

    let classifier = RouteClassifier::new()
        .expensive("/v1/sync/{provider}/pull")
        .expensive("/v1/sync/{provider}/push")
        .expensive("/v1/sync/push-series")
        .expensive("/v1/sync/enrich");
    let limiter = RateLimiter::from_config(&cfg.rate_limit, classifier, None);

    let app = HttpStack::new(&cfg.security, metrics.clone())
        .with_rate_limit(limiter)
        .with_internal_auth(Some(tankovault_service::InternalAuth::new(
            internal_auth,
            tankovault_service::RouteTable(INTERNAL_ROUTES),
        )))
        .apply(routes)
        .merge(tankovault_service::ops_router(health.clone(), metrics));

    tankovault_service::serve_internal(
        &cfg.bind_addr,
        app,
        tankovault_service::probe_router(health),
        internal_auth,
        shutdown,
    )
    .await?;
    Ok(())
}

async fn providers_list(State(state): State<AppState>) -> Json<Vec<ProviderInfo>> {
    Json(state.engine.registry())
}

/// `POST /v1/sync/enrich` — run one tokenless metadata-enrichment sweep on demand (no user
/// token). Uses the configured batch/cap. Handy for ops and for kicking a fresh sweep after
/// a bulk import rather than waiting for the periodic worker.
async fn enrich(State(state): State<AppState>) -> Result<Json<engine::EnrichReport>, AppError> {
    let report = state
        .engine
        .enrich_all(state.enrich_batch, state.enrich_max)
        .await?;
    Ok(Json(report))
}

#[derive(Debug, Deserialize)]
struct PushSeriesRequest {
    user_id: UserId,
    series_id: SeriesId,
}

/// Always `200`, even when every provider push failed — failures are reported per-provider in
/// the body (and recorded to `external_accounts.last_error`), never surfaced as an HTTP error,
/// since this is called fire-and-forget from the API.
async fn push_series(
    State(state): State<AppState>,
    Json(req): Json<PushSeriesRequest>,
) -> Json<Vec<engine::ProviderPushOutcome>> {
    Json(state.engine.push_series(req.user_id, req.series_id).await)
}

async fn authorize_url(
    State(state): State<AppState>,
    Path(provider): Path<String>,
) -> Result<Json<AuthorizeUrl>, AppError> {
    Ok(Json(AuthorizeUrl {
        url: state.engine.authorize_url(&provider)?,
    }))
}

#[derive(Debug, Deserialize)]
struct LinkRequest {
    user_id: UserId,
    code: String,
}

async fn link(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Json(req): Json<LinkRequest>,
) -> Result<StatusCode, AppError> {
    state.engine.link(&provider, req.user_id, &req.code).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
struct UserRequest {
    user_id: UserId,
}

/// `GET /v1/sync/{provider}/status/{user_id}` — always `200`; `linked: false` when unlinked.
async fn status(
    State(state): State<AppState>,
    Path((provider, user_id)): Path<(String, UserId)>,
) -> Result<Json<AccountStatus>, AppError> {
    Ok(Json(state.engine.status(&provider, user_id).await?))
}

async fn unlink(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Json(req): Json<UserRequest>,
) -> Result<Json<Removed>, AppError> {
    let removed = state.engine.unlink(&provider, req.user_id).await?;
    Ok(Json(Removed { removed }))
}

#[derive(Debug, Deserialize)]
struct SyncRequest {
    user_id: UserId,
    #[serde(default)]
    policy: Option<ConflictPolicy>,
}

async fn pull(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Json(req): Json<SyncRequest>,
) -> Result<Json<engine::PullReport>, AppError> {
    let report = state
        .engine
        .pull(&provider, req.user_id, req.policy)
        .await?;
    Ok(Json(report))
}

async fn push(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Json(req): Json<SyncRequest>,
) -> Result<Json<engine::PushReport>, AppError> {
    let report = state
        .engine
        .push(&provider, req.user_id, req.policy)
        .await?;
    Ok(Json(report))
}

/// `GET /v1/sync/{provider}/settings/{user_id}` — the account's automatic-sync settings.
async fn get_settings(
    State(state): State<AppState>,
    Path((provider, user_id)): Path<(String, UserId)>,
) -> Result<Json<AccountSettings>, AppError> {
    Ok(Json(state.engine.settings(&provider, user_id).await?))
}

#[derive(Debug, Deserialize)]
struct SettingsPatch {
    user_id: UserId,
    #[serde(default)]
    auto_sync_enabled: Option<bool>,
    #[serde(default)]
    conflict_policy: Option<ConflictPolicy>,
}

/// `PATCH /v1/sync/{provider}/settings/{user_id}` — update automatic-sync settings.
async fn patch_settings(
    State(state): State<AppState>,
    Path((provider, _user_id)): Path<(String, UserId)>,
    Json(req): Json<SettingsPatch>,
) -> Result<StatusCode, AppError> {
    state
        .engine
        .update_settings(
            &provider,
            req.user_id,
            req.auto_sync_enabled,
            req.conflict_policy,
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /v1/sync/conflicts/{user_id}` — the user's pending conflicts across all providers.
async fn list_conflicts(
    State(state): State<AppState>,
    Path(user_id): Path<UserId>,
) -> Result<Json<Vec<ConflictView>>, AppError> {
    let rows = state.engine.list_conflicts(user_id).await?;
    Ok(Json(rows.into_iter().map(views::conflict_view).collect()))
}

#[derive(Debug, Deserialize)]
struct ResolveRequest {
    user_id: UserId,
    resolution: String,
}

/// `POST /v1/sync/conflicts/{id}/resolve` — apply a user's chosen resolution.
async fn resolve_conflict(
    State(state): State<AppState>,
    Path(id): Path<uuid::Uuid>,
    Json(req): Json<ResolveRequest>,
) -> Result<Json<Resolved>, AppError> {
    let resolved = state
        .engine
        .resolve_conflict(req.user_id, id, &req.resolution)
        .await?;
    Ok(Json(Resolved { resolved }))
}

#[derive(Debug, Deserialize)]
struct HistoryQuery {
    #[serde(default)]
    series_id: Option<SeriesId>,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    page: Option<i64>,
}

/// `GET /v1/sync/history/{user_id}` — a page of the user's sync history.
async fn list_history(
    State(state): State<AppState>,
    Path(user_id): Path<UserId>,
    axum::extract::Query(q): axum::extract::Query<HistoryQuery>,
) -> Result<Json<Vec<HistoryView>>, AppError> {
    let rows = state
        .engine
        .history(
            user_id,
            q.series_id,
            q.provider.as_deref(),
            q.page.unwrap_or(0),
        )
        .await?;
    Ok(Json(rows.into_iter().map(views::history_view).collect()))
}

#[derive(Debug, Deserialize)]
struct DecisionQuery {
    #[serde(default)]
    user_id: Option<UserId>,
    #[serde(default)]
    series_id: Option<SeriesId>,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    action: Option<String>,
    #[serde(default)]
    run_id: Option<uuid::Uuid>,
    #[serde(default)]
    applied_only: bool,
    #[serde(default)]
    flagged_only: bool,
    #[serde(default)]
    limit: Option<i64>,
    #[serde(default)]
    offset: Option<i64>,
}

/// `GET /v1/sync/decisions` — the operator-facing journal of what the engine decided and why.
async fn list_decisions(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<DecisionQuery>,
) -> Result<Json<Vec<tankovault_contracts::admin::SyncDecisionView>>, AppError> {
    let filter = tankovault_db::repo::sync::SyncDecisionFilter {
        user_id: q.user_id,
        series_id: q.series_id,
        provider: q.provider,
        action: q.action,
        run_id: q.run_id,
        applied_only: q.applied_only,
        flagged_only: q.flagged_only,
    };
    let rows = state
        .engine
        .list_decisions(
            &filter,
            q.limit.unwrap_or(100).clamp(1, 500),
            q.offset.unwrap_or(0),
        )
        .await?;
    Ok(Json(rows.into_iter().map(views::decision_view).collect()))
}

#[derive(Debug, Deserialize)]
struct RevertRequest {
    /// The operator asking, for attribution. `None` for an automated caller.
    #[serde(default)]
    actor: Option<UserId>,
    reason: String,
}

/// `POST /v1/sync/decisions/{id}/revert` — undo one journalled decision.
async fn revert_decision(
    State(state): State<AppState>,
    Path(id): Path<uuid::Uuid>,
    Json(req): Json<RevertRequest>,
) -> Result<Json<engine::RevertReport>, AppError> {
    let report = state
        .engine
        .revert_decision(id, req.actor, &req.reason)
        .await?;
    Ok(Json(report))
}

#[derive(Debug, Deserialize)]
struct FlagRequest {
    #[serde(default)]
    actor: Option<UserId>,
    reason: String,
    /// Also refuse the (external id, series) match this decision made, permanently.
    #[serde(default)]
    block_match: bool,
}

/// `POST /v1/sync/decisions/{id}/flag` — mark one decision wrong without undoing it.
async fn flag_decision(
    State(state): State<AppState>,
    Path(id): Path<uuid::Uuid>,
    Json(req): Json<FlagRequest>,
) -> Result<Json<Flagged>, AppError> {
    let flagged = state
        .engine
        .flag_decision(id, req.actor, &req.reason, req.block_match)
        .await?;
    Ok(Json(Flagged { flagged }))
}

#[derive(Debug, Deserialize)]
struct BlockMatchRequest {
    provider: String,
    external_id: String,
    series_id: SeriesId,
    #[serde(default)]
    actor: Option<UserId>,
    reason: String,
}

/// `POST /v1/sync/match-blocks` — refuse one title match permanently, without a decision row.
async fn block_match(
    State(state): State<AppState>,
    Json(req): Json<BlockMatchRequest>,
) -> Result<StatusCode, AppError> {
    state
        .engine
        .block_match(
            &req.provider,
            &req.external_id,
            req.series_id,
            req.actor,
            &req.reason,
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod route_feature_tests {
    use super::route_features;
    use axum::http::Method;

    /// Every suffix in the shared declaration is actually gated under this tier's prefix.
    /// Mirrors the same test in `services/api`; the two tiers' tables had already drifted once
    /// with nothing asserting they agreed.
    #[test]
    fn the_shared_sync_declaration_is_applied_under_this_tier_s_prefix() {
        let features = route_features();
        for (suffix, expected) in tankovault_contracts::sync::sync_route_features() {
            let path = format!("/v1/sync{suffix}");
            assert_eq!(
                features.required(&Method::GET, &path),
                Some(*expected),
                "{path} is not gated by the feature the shared declaration names"
            );
        }
    }
}
