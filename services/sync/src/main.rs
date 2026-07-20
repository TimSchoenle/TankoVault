//! # sync service (`AniList`)
//!
//! External-sync microservice (design §15). Owns the `AniList` `OAuth2` flow, encrypts tokens
//! at rest, and reconciles the user's `AniList` manga list with the local watchlist/progress
//! using the shared `matcher`. The user-facing `/v1/me/sync/anilist/*` routes on the API
//! delegate here; this service exposes the internal contract below.
//!
//! ```text
//! GET    /health | /ready
//! GET    /v1/anilist/authorize-url        -> { url }
//! POST   /v1/anilist/link    { user_id, code }            -> 204
//! DELETE /v1/anilist/link    { user_id }                  -> { removed }
//! POST   /v1/anilist/pull    { user_id, policy? }         -> PullReport
//! POST   /v1/anilist/push    { user_id, policy? }         -> PushReport
//! ```

mod anilist;
mod engine;
mod mapping;

use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;

use anilist::{AniListClient, DEFAULT_GRAPHQL_URL, DEFAULT_OAUTH_BASE};
use engine::SyncEngine;
use tankovault_auth::SecretBox;
use tankovault_config::{DatabaseConfig, TelemetryConfig};
use tankovault_domain::UserId;
use mapping::ConflictPolicy;

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

    let client = AniListClient::new(
        cfg.anilist.graphql_url,
        cfg.anilist.oauth_base,
        cfg.anilist.client_id,
        cfg.anilist.client_secret,
        cfg.anilist.redirect_uri,
        Duration::from_millis(cfg.anilist.min_request_interval_ms),
    )?;

    let engine = Arc::new(SyncEngine::new(
        pool,
        client,
        secret,
        cfg.anilist.default_conflict_policy,
    ));
    let state = AppState { engine };

    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/ready", get(|| async { "ok" }))
        .route("/v1/anilist/authorize-url", get(authorize_url))
        .route("/v1/anilist/link", post(link).delete(unlink))
        .route("/v1/anilist/pull", post(pull))
        .route("/v1/anilist/push", post(push))
        .with_state(state);

    let listener = TcpListener::bind(&cfg.bind_addr).await?;
    tracing::info!(addr = %cfg.bind_addr, "sync service listening");
    axum::serve(listener, app).await?;
    Ok(())
}

#[derive(Debug, Serialize)]
struct AuthorizeUrl {
    url: String,
}

async fn authorize_url(State(state): State<AppState>) -> Json<AuthorizeUrl> {
    Json(AuthorizeUrl {
        url: state.engine.authorize_url(),
    })
}

#[derive(Debug, Deserialize)]
struct LinkRequest {
    user_id: UserId,
    code: String,
}

async fn link(
    State(state): State<AppState>,
    Json(req): Json<LinkRequest>,
) -> Result<StatusCode, AppError> {
    state.engine.link(req.user_id, &req.code).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
struct UserRequest {
    user_id: UserId,
}

#[derive(Debug, Serialize)]
struct Removed {
    removed: bool,
}

async fn unlink(
    State(state): State<AppState>,
    Json(req): Json<UserRequest>,
) -> Result<Json<Removed>, AppError> {
    let removed = state.engine.unlink(req.user_id).await?;
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
    Json(req): Json<SyncRequest>,
) -> Result<Json<engine::PullReport>, AppError> {
    let report = state.engine.pull(req.user_id, req.policy).await?;
    Ok(Json(report))
}

async fn push(
    State(state): State<AppState>,
    Json(req): Json<SyncRequest>,
) -> Result<Json<engine::PushReport>, AppError> {
    let report = state.engine.push(req.user_id, req.policy).await?;
    Ok(Json(report))
}

/// Thin error wrapper: surfaces the message to the caller (the API) and a `502` since most
/// failures originate upstream at `AniList`; a missing link is reported as `409`.
struct AppError(anyhow::Error);

impl From<anyhow::Error> for AppError {
    fn from(err: anyhow::Error) -> Self {
        Self(err)
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        let message = self.0.to_string();
        let status = if message.contains("no AniList account linked") {
            StatusCode::CONFLICT
        } else {
            StatusCode::BAD_GATEWAY
        };
        tracing::warn!(error = %message, "sync request failed");
        (status, message).into_response()
    }
}
