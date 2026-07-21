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

mod anilist;
mod engine;
mod mapping;
mod provider;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;

use anilist::{AniListClient, DEFAULT_GRAPHQL_URL, DEFAULT_OAUTH_BASE};
use engine::SyncEngine;
use mapping::ConflictPolicy;
use provider::ExternalProvider;
use tankovault_auth::SecretBox;
use tankovault_config::{DatabaseConfig, TelemetryConfig};
use tankovault_domain::{SeriesId, UserId};

#[derive(Debug, Deserialize)]
struct Config {
    database: DatabaseConfig,
    telemetry: TelemetryConfig,
    anilist: AniListConfig,
    #[serde(default = "default_bind")]
    bind_addr: String,
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
    providers.insert(anilist::PROVIDER, Box::new(anilist));
    Ok(providers)
}

#[derive(Clone)]
struct AppState {
    engine: Arc<SyncEngine>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cfg: Config = tankovault_config::load()?;
    tankovault_observability::init_tracing(&cfg.telemetry)?;

    let pool = tankovault_db::connect(
        &cfg.database.url,
        cfg.database.max_connections,
        cfg.database.acquire_timeout_secs,
    )
    .await?;

    let secret = SecretBox::from_base64_key(&cfg.anilist.token_encryption_key)
        .map_err(|e| anyhow::anyhow!("invalid anilist.token_encryption_key: {e}"))?;
    let default_policy = cfg.anilist.default_conflict_policy;
    let providers = build_providers(cfg.anilist)?;

    let engine = Arc::new(SyncEngine::new(pool, secret, default_policy, providers));
    let state = AppState { engine };

    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/ready", get(|| async { "ok" }))
        .route("/v1/sync/providers", get(providers_list))
        .route("/v1/sync/push-series", post(push_series))
        .route("/v1/sync/{provider}/authorize-url", get(authorize_url))
        .route("/v1/sync/{provider}/status/{user_id}", get(status))
        .route("/v1/sync/{provider}/link", post(link).delete(unlink))
        .route("/v1/sync/{provider}/pull", post(pull))
        .route("/v1/sync/{provider}/push", post(push))
        .with_state(state);

    let listener = TcpListener::bind(&cfg.bind_addr).await?;
    tracing::info!(addr = %cfg.bind_addr, "sync service listening");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn providers_list(State(state): State<AppState>) -> Json<Vec<provider::ProviderInfo>> {
    Json(state.engine.registry())
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

#[derive(Debug, Serialize)]
struct AuthorizeUrl {
    url: String,
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
) -> Result<Json<engine::AccountStatus>, AppError> {
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

/// Thin error wrapper: surfaces the message to the caller (the API) and a `502` since most
/// failures originate upstream at the provider; an unknown provider slug is `404`, a missing
/// link is `409`.
struct AppError(anyhow::Error);

impl From<anyhow::Error> for AppError {
    fn from(err: anyhow::Error) -> Self {
        Self(err)
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        let message = self.0.to_string();
        let status = if message.contains("unknown sync provider") {
            StatusCode::NOT_FOUND
        } else if message.contains("account linked") {
            StatusCode::CONFLICT
        } else {
            StatusCode::BAD_GATEWAY
        };
        tracing::warn!(error = %message, "sync request failed");
        (status, message).into_response()
    }
}
