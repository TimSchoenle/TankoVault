//! External-sync microservice: owns each provider's `OAuth2` flow, encrypts tokens at rest, and
//! reconciles a user's remote list with the local watchlist/progress. The user-facing
//! `/v1/me/sync/{provider}/*` routes on the API delegate to the internal contract exposed here.

mod engine;
mod error;
mod mapping;
mod provider;
mod providers;
/// Merge-engine reconciliation tests; needs Docker, hence the feature gate.
#[cfg(all(test, feature = "integration"))]
mod reconcile_tests;

use crate::error::AppError;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use secrecy::{ExposeSecret as _, SecretString};
use serde::{Deserialize, Serialize};
use tankovault_service::health::PostgresCheck;
use tankovault_service::{
    FeatureGate, FeatureLayer, Health, HttpStack, MetricsRegistry, PostgresFlagSource, RateLimiter,
    RouteClassifier, RouteFeatures,
};

use engine::SyncEngine;
use mapping::ConflictPolicy;
use provider::ExternalProvider;
use providers::anilist::{AniListClient, DEFAULT_GRAPHQL_URL, DEFAULT_OAUTH_BASE};
use tankovault_auth::Sealer;
use tankovault_config::{DatabaseConfig, TelemetryConfig};
use tankovault_contracts::sync::{AccountSettings, AccountStatus, AuthorizeUrl, ProviderInfo};
use tankovault_domain::{Feature, MetadataPriority, SeriesId, UserId};

#[derive(Debug, Deserialize)]
struct Config {
    database: DatabaseConfig,
    telemetry: TelemetryConfig,
    anilist: AniListConfig,
    #[serde(default)]
    metadata: MetadataConfig,
    #[serde(default = "default_bind")]
    bind_addr: String,
    /// Interval (seconds) between scheduled reconciliation ticks. `0` disables the loop.
    #[serde(default = "default_reconcile_interval")]
    reconcile_interval_secs: u64,
    /// Edge hardening for this internal service.
    #[serde(default)]
    security: tankovault_config::SecurityConfig,
    /// Inbound rate limiting; pull/push routes draw from the tighter "expensive" budget.
    #[serde(default)]
    rate_limit: tankovault_config::RateLimitConfig,
    /// Prometheus metrics. Togglable; disabling installs no recorder.
    #[serde(default)]
    metrics: tankovault_config::MetricsConfig,
    /// Runtime feature flags — how often this replica re-reads the operator's decisions.
    #[serde(default)]
    features: tankovault_config::FeaturesConfig,
    /// Shared secret every caller must present: this whole contract is privileged, naming the
    /// subject user in the path or body.
    #[serde(default)]
    internal: tankovault_config::InternalAuthConfig,
    /// The confidence policy for resolving a remote entry onto a local series. Shared with the
    /// worker's ingest canonicalisation so the two paths can't disagree on a match.
    #[serde(default)]
    matching: tankovault_config::MatchingConfig,
}

/// Metadata-priority + tokenless enrichment-worker settings.
#[derive(Debug, Deserialize)]
struct MetadataConfig {
    /// Per-field source authority order (default: `AniList` before the adapters).
    #[serde(default)]
    priority: MetadataPriority,
    /// Whether the background enrichment worker runs. On by default.
    #[serde(default = "default_enrich_enabled")]
    enrich_enabled: bool,
    /// Seconds between enrichment sweeps.
    #[serde(default = "default_enrich_interval_secs")]
    enrich_interval_secs: u64,
    /// Series fetched per DB page during a sweep.
    #[serde(default = "default_enrich_batch")]
    enrich_batch: i64,
    /// Upper bound on series processed per sweep (paces `AniList`'s rate limit).
    #[serde(default = "default_enrich_max")]
    enrich_max_series: usize,
}

impl Default for MetadataConfig {
    fn default() -> Self {
        Self {
            priority: MetadataPriority::default(),
            enrich_enabled: default_enrich_enabled(),
            enrich_interval_secs: default_enrich_interval_secs(),
            enrich_batch: default_enrich_batch(),
            enrich_max_series: default_enrich_max(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct AniListConfig {
    #[serde(deserialize_with = "string_or_number")]
    client_id: String,
    /// The `OAuth2` client secret; lets anyone mint tokens as this app.
    client_secret: SecretString,
    redirect_uri: String,
    /// Base64 32-byte data-encryption key for tokens at rest — opens every user's stored
    /// `AniList` access and refresh token.
    token_encryption_key: SecretString,
    #[serde(default = "default_graphql_url")]
    graphql_url: String,
    #[serde(default = "default_oauth_base")]
    oauth_base: String,
    #[serde(default)]
    default_conflict_policy: ConflictPolicy,
    #[serde(default = "default_min_interval_ms")]
    min_request_interval_ms: u64,
}

fn default_bind() -> String {
    "0.0.0.0:8083".to_owned()
}
fn default_graphql_url() -> String {
    DEFAULT_GRAPHQL_URL.to_owned()
}
fn default_oauth_base() -> String {
    DEFAULT_OAUTH_BASE.to_owned()
}
fn default_min_interval_ms() -> u64 {
    700
}
fn default_reconcile_interval() -> u64 {
    900
}
fn default_enrich_enabled() -> bool {
    true
}
fn default_enrich_interval_secs() -> u64 {
    3600
}
fn default_enrich_batch() -> i64 {
    200
}
fn default_enrich_max() -> usize {
    // Must stay comfortably inside one sweep interval at `min_request_interval_ms` pacing, or
    // sweeps overlap; too low and metadata visibly lags for days.
    2_000
}

/// `figment`'s `Env` provider infers numeric-looking values (e.g. `TANKOVAULT_ANILIST__CLIENT_ID`)
/// as numbers rather than strings, so accept either and coerce to `String`.
fn string_or_number<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrNumber {
        String(String),
        Int(i64),
        UInt(u64),
        Float(f64),
    }

    Ok(match StringOrNumber::deserialize(deserializer)? {
        StringOrNumber::String(s) => s,
        StringOrNumber::Int(i) => i.to_string(),
        StringOrNumber::UInt(u) => u.to_string(),
        StringOrNumber::Float(f) => f.to_string(),
    })
}

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
    // Before anything else: `scratch` images have no shell/wget, so a Docker HEALTHCHECK
    // invokes this binary itself as the only available probe.
    if tankovault_service::healthcheck::requested() {
        let cfg: Config = tankovault_config::load()?;
        tankovault_service::run_healthcheck_and_exit(&cfg.bind_addr);
    }

    let cfg: Config = tankovault_config::load()?;
    tankovault_service::init_tracing(&cfg.telemetry)?;
    let metrics = MetricsRegistry::install(&cfg.metrics)?;
    // Resolved before anything binds: production refuses to boot rather than silently serving
    // privileged routes without the token that guards them.
    let internal_token = tankovault_service::internal_auth::resolve(&cfg.internal)?;
    let shutdown = tankovault_service::install_shutdown();

    let pool = tankovault_db::connect(
        &cfg.database.url,
        cfg.database.max_connections,
        cfg.database.acquire_timeout_secs,
    )
    .await?;
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
    let metadata = cfg.metadata;
    let providers = build_providers(cfg.anilist)?;

    let engine = Arc::new(SyncEngine::new(
        pool,
        secret,
        default_policy,
        metadata.priority.clone(),
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
        .route("/v1/sync/providers", get(providers_list))
        .route("/v1/sync/push-series", post(push_series))
        .route("/v1/sync/enrich", post(enrich))
        .route("/v1/sync/{provider}/authorize-url", get(authorize_url))
        .route("/v1/sync/{provider}/status/{user_id}", get(status))
        .route("/v1/sync/{provider}/link", post(link).delete(unlink))
        .route("/v1/sync/{provider}/pull", post(pull))
        .route("/v1/sync/{provider}/push", post(push))
        .route(
            "/v1/sync/{provider}/settings/{user_id}",
            get(get_settings).patch(patch_settings),
        )
        .route("/v1/sync/conflicts/{user_id}", get(list_conflicts))
        .route("/v1/sync/conflicts/{id}/resolve", post(resolve_conflict))
        .route("/v1/sync/history/{user_id}", get(list_history))
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

    tankovault_service::spawn_metrics_server(metrics.clone(), shutdown.clone());

    let app = HttpStack::new(&cfg.security, metrics.clone())
        .with_rate_limit(limiter)
        .with_internal_auth(internal_token)
        .apply(routes)
        .merge(tankovault_service::ops_router(health, metrics));

    tankovault_service::serve(&cfg.bind_addr, app, shutdown).await?;
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

#[derive(Debug, Serialize)]
struct Removed {
    removed: bool,
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
) -> Result<Json<Vec<tankovault_db::repo::sync::ConflictRow>>, AppError> {
    Ok(Json(state.engine.list_conflicts(user_id).await?))
}

#[derive(Debug, Deserialize)]
struct ResolveRequest {
    user_id: UserId,
    resolution: String,
}

#[derive(Debug, Serialize)]
struct Resolved {
    resolved: bool,
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
) -> Result<Json<Vec<tankovault_db::repo::sync::HistoryRow>>, AppError> {
    let rows = state
        .engine
        .history(
            user_id,
            q.series_id,
            q.provider.as_deref(),
            q.page.unwrap_or(0),
        )
        .await?;
    Ok(Json(rows))
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
