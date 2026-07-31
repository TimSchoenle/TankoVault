//! # sync service (external trackers)
//!
//! External-sync microservice (design §15, generalized to a provider registry). Owns each
//! provider's `OAuth2` flow, encrypts tokens at rest, and reconciles a user's remote list with
//! the local watchlist/progress using the shared `matcher`. `AniList` is the only registered
//! provider today; a second provider is a drop-in `ExternalProvider` implementation registered in
//! [`build_providers`]. The user-facing `/v1/me/sync/{provider}/*` routes on the API delegate
//! here; this service exposes the internal contract below.
//!
//! ```text
//! GET    /health | /ready
//! GET    /v1/sync/providers                                -> Vec<ProviderInfo>
//! POST   /v1/sync/push-series  { user_id, series_id }       -> Vec<ProviderPushOutcome> (always 200)
//! GET    /v1/sync/{provider}/authorize-url                  -> { url }
//! GET    /v1/sync/{provider}/status/{user_id}               -> AccountStatus
//! POST   /v1/sync/{provider}/link    { user_id, code }      -> 204
//! DELETE /v1/sync/{provider}/link    { user_id }            -> { removed }
//! POST   /v1/sync/{provider}/pull    { user_id, policy? }   -> PullReport
//! POST   /v1/sync/{provider}/push    { user_id, policy? }   -> PushReport
//! ```
//!
//! `anilist.redirect_uri` (config) must point at a **frontend** page, not at this service or
//! the API directly: the API's `/v1/me/sync/{provider}/callback` requires the caller's Bearer
//! access token, which only exists in the SPA's in-memory session and cannot ride along on
//! the browser's raw OAuth redirect. The frontend's callback route reads `?code=` from the
//! URL and then calls that API endpoint itself, attaching the token like any other request.

mod engine;
mod error;
mod mapping;
mod provider;
mod providers;
/// Merge-engine reconciliation tests (audit TEST F-06). Needs Docker, hence the feature gate.
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
use tankovault_auth::SecretBox;
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
    /// Interval (seconds) between scheduled reconciliation ticks (design v2 §B.4). `0`
    /// disables the loop (e.g. in tests or when a separate scheduler owns it).
    #[serde(default = "default_reconcile_interval")]
    reconcile_interval_secs: u64,
    /// Edge hardening for this internal service.
    #[serde(default)]
    security: tankovault_config::SecurityConfig,
    /// Inbound rate limiting. The pull/push routes each fan out to a third-party API, so
    /// they draw from the tighter "expensive" budget.
    #[serde(default)]
    rate_limit: tankovault_config::RateLimitConfig,
    /// Prometheus metrics. Togglable; disabling installs no recorder.
    #[serde(default)]
    metrics: tankovault_config::MetricsConfig,
    /// Runtime feature flags — how often this replica re-reads the operator's decisions.
    #[serde(default)]
    features: tankovault_config::FeaturesConfig,
    /// Shared secret every caller must present. This service's whole contract is
    /// privileged — it names the subject user in the path or body — so an unauthenticated
    /// caller could read or rewrite any account's sync state.
    #[serde(default)]
    internal: tankovault_config::InternalAuthConfig,
    /// The confidence policy for resolving a remote entry onto a local series. Shared with the
    /// worker's ingest canonicalisation so the two paths cannot disagree about whether two
    /// series are the same (ARCH-16).
    #[serde(default)]
    matching: tankovault_config::MatchingConfig,
}

/// Metadata-priority + tokenless enrichment-worker settings (design: worker queue syncing
/// every existing entry to `AniList` without a stored user token, plus a configurable
/// authority order for description/cover/title).
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
    client_secret: String,
    redirect_uri: String,
    /// Base64 (standard alphabet) 32-byte data-encryption key for tokens at rest.
    token_encryption_key: String,
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
    500
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

/// Build the provider registry. `AniList` is the only entry today; register additional
/// providers here as they land — no other wiring changes needed.
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

/// Tokenless metadata-enrichment worker: a periodic background sweep that fills in
/// description/cover/alternative-titles for every existing series straight from `AniList`'s
/// public API — no stored user token required (design: worker queue syncing all entries).
///
/// Not feature-gated. It reads public metadata about the *catalogue*, not about any user, and
/// belongs to the catalogue rather than to external sync; switching `sync.external` off should
/// not stop series descriptions being filled in.
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

/// Scheduled reconciliation loop (design v2 §B.4): pulls remote-side changes back
/// automatically, closing the reactive-push-only gap. Disabled when `interval_secs` is 0.
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
                // Checked per tick, not at boot: this loop reaches out to third parties on
                // every user's behalf, and an operator stopping that — because a provider is
                // rate-limiting, or because a bad mapping is propagating — must take effect
                // now. The loop keeps ticking so it resumes on its own when the flag returns.
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
/// `tankovault_contracts::sync` (ARCH-18).
///
/// The API gates the same surface under `/v1/me/sync` from the same list. Maintaining the two
/// tables independently had already let them drift: the API gated `/conflicts` and `/history` but
/// not `/push-series`, and nothing asserted they agreed.
fn route_features() -> RouteFeatures {
    tankovault_contracts::sync::sync_route_features()
        .iter()
        .fold(RouteFeatures::new(), |table, (suffix, feature)| {
            table.gate(format!("/v1/sync{suffix}"), *feature)
        })
}

/// Whether `encoded` is a key made entirely of zero bytes.
///
/// Compares the *decoded* bytes rather than the string, so the several base64 spellings of 32
/// zero bytes (with and without padding, with whitespace) are all caught rather than only the
/// exact literal that happened to ship in the compose file.
fn is_placeholder_key(encoded: &str) -> bool {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .is_ok_and(|bytes| !bytes.is_empty() && bytes.iter().all(|b| *b == 0))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Before config, telemetry or anything else: this process may have been invoked by
    // Docker's HEALTHCHECK rather than as the service. `scratch` images have no shell and no
    // wget, so the binary probing itself is the only probe available. See
    // `tankovault_service::healthcheck`.
    if tankovault_service::healthcheck::requested() {
        let cfg: Config = tankovault_config::load()?;
        tankovault_service::run_healthcheck_and_exit(&cfg.bind_addr);
    }

    let cfg: Config = tankovault_config::load()?;
    tankovault_service::init_tracing(&cfg.telemetry)?;
    let metrics = MetricsRegistry::install(&cfg.metrics)?;
    // Resolved before anything binds: a service in this tier that starts without a token
    // silently downgrades to the unauthenticated behaviour the token exists to remove, so
    // the production profile refuses to boot rather than serving privileged routes openly.
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

    // Refused in *every* profile, like the API's JWT placeholder check. This key seals every
    // user's AniList access and refresh token at rest, and the published fallback was 32 zero
    // bytes — a key anyone who has read `deploy/docker-compose.yml` already holds. The length
    // is enforced by `from_base64_key` itself (it decodes into a `[u8; 32]`), so the only
    // remaining hole was a well-known *value*.
    if is_placeholder_key(&cfg.anilist.token_encryption_key) {
        anyhow::bail!(
            "refusing to start: anilist.token_encryption_key is the all-zero placeholder \
             published in this repository. Every stored OAuth token sealed with it is \
             readable by anyone with a copy of the database. Generate one with \
             `openssl rand -base64 32` and set TANKOVAULT_ANILIST__TOKEN_ENCRYPTION_KEY."
        );
    }
    let secret = SecretBox::from_base64_key(&cfg.anilist.token_encryption_key)
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
        // The same declarative gate the public API uses, applied to this service's own
        // contract. The API already refuses these routes when `sync.external` is off, but this
        // service is reachable from anywhere on the internal network and a switch an operator
        // has thrown should hold where the work happens, not only at the edge in front of it.
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
    ///
    /// The mirror of the same test in `services/api`. The two tiers used to keep independent
    /// tables and had already drifted — the API gated `/conflicts` and `/history` but not
    /// `/push-series` — with nothing asserting they agreed (ARCH-18).
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
