//! External-sync proxy endpoints, keyed by provider.

use crate::error::{ApiError, ApiResult};
use crate::openapi::ME_SYNC_TAG;
use crate::state::{AppState, AuthUser};
use axum::Json;
use axum::extract::{Path, Query, State};
use serde::Deserialize;
use std::fmt::Write as _;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

/// List sync providers
///
/// The registered external providers (design: generalized multi-provider sync). Drives the
/// Account "Sync & integrations" panel, which renders one card per entry instead of a single
/// hardcoded `AniList` block. The body type is shared with the sync service via
/// `tankovault_contracts::sync`, so the generated client exposes this endpoint typed.
#[utoipa::path(
    get,
    path = "/v1/me/sync/providers",
    tag = ME_SYNC_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Providers, forwarded from the sync service", body = Vec<tankovault_contracts::sync::ProviderInfo>),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn sync_providers(
    State(state): State<AppState>,
    _user: AuthUser,
) -> ApiResult<Json<serde_json::Value>> {
    let url = format!("{}/v1/sync/providers", state.sync_url.trim_end_matches('/'));
    let resp = state.http.get(url).send().await.map_err(|e| {
        tracing::error!(error = %e, "sync service unreachable");
        ApiError::Internal
    })?;
    if !resp.status().is_success() {
        return Err(ApiError::Internal);
    }
    Ok(Json(resp.json().await.map_err(|_| ApiError::Internal)?))
}

/// Get a provider's OAuth consent URL
///
/// Returns the provider's consent URL (proxied). The body type is shared with the sync
/// service via `tankovault_contracts::sync`.
#[utoipa::path(
    get,
    path = "/v1/me/sync/{provider}/authorize",
    tag = ME_SYNC_TAG,
    params(("provider" = String, Path, description = "Provider slug")),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Consent URL, forwarded from the sync service", body = tankovault_contracts::sync::AuthorizeUrl),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn sync_authorize_url(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(provider): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let url = format!(
        "{}/v1/sync/{provider}/authorize-url",
        state.sync_url.trim_end_matches('/')
    );
    let resp = state.http.get(url).send().await.map_err(|e| {
        tracing::error!(error = %e, "sync service unreachable");
        ApiError::Internal
    })?;
    if !resp.status().is_success() {
        return Err(ApiError::Internal);
    }
    Ok(Json(resp.json().await.map_err(|_| ApiError::Internal)?))
}

/// Get link status for a provider
///
/// Whether the caller has a linked account at `provider`, plus the connected display name and
/// most recent sync time (Sync & integrations panel, header pill, Series tracking card).
/// Always `200`; an unlinked account reads `{ "linked": false }`. The body type is shared with
/// the sync service via `tankovault_contracts::sync`.
#[utoipa::path(
    get,
    path = "/v1/me/sync/{provider}/status",
    tag = ME_SYNC_TAG,
    params(("provider" = String, Path, description = "Provider slug")),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Link status, forwarded from the sync service", body = tankovault_contracts::sync::AccountStatus),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn sync_status(
    State(state): State<AppState>,
    user: AuthUser,
    Path(provider): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let url = format!(
        "{}/v1/sync/{provider}/status/{}",
        state.sync_url.trim_end_matches('/'),
        user.user_id.as_uuid()
    );
    let resp = state.http.get(url).send().await.map_err(|e| {
        tracing::error!(error = %e, "sync service unreachable");
        ApiError::Internal
    })?;
    if !resp.status().is_success() {
        return Err(ApiError::Internal);
    }
    Ok(Json(resp.json().await.map_err(|_| ApiError::Internal)?))
}

/// Unlink a provider
///
/// Unlink the caller's account at `provider`. Response shape is defined by the sync service
/// and forwarded verbatim; not tracked here.
#[utoipa::path(
    delete,
    path = "/v1/me/sync/{provider}",
    tag = ME_SYNC_TAG,
    params(("provider" = String, Path, description = "Provider slug")),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Unlinked, forwarded from the sync service"),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn sync_disconnect(
    State(state): State<AppState>,
    user: AuthUser,
    Path(provider): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let url = format!(
        "{}/v1/sync/{provider}/link",
        state.sync_url.trim_end_matches('/')
    );
    let resp = state
        .http
        .delete(url)
        .json(&serde_json::json!({ "user_id": user.user_id }))
        .send()
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "sync service unreachable");
            ApiError::Internal
        })?;
    if !resp.status().is_success() {
        return Err(ApiError::Internal);
    }
    Ok(Json(resp.json().await.map_err(|_| ApiError::Internal)?))
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct AniListCallback {
    pub code: String,
}

/// Complete an OAuth link
///
/// Exchanges the authorization `code` and links the caller's account at `provider`. Response
/// shape is defined by the sync service and forwarded verbatim; not tracked here.
#[utoipa::path(
    get,
    path = "/v1/me/sync/{provider}/callback",
    tag = ME_SYNC_TAG,
    params(("provider" = String, Path, description = "Provider slug"), AniListCallback),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Linked, forwarded from the sync service"),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 404, description = "Unknown provider", body = crate::error::ProblemDetails),
        (status = 409, description = "Account not linked", body = crate::error::ProblemDetails),
    )
)]
pub async fn sync_callback(
    State(state): State<AppState>,
    user: AuthUser,
    Path(provider): Path<String>,
    Query(q): Query<AniListCallback>,
) -> ApiResult<Json<serde_json::Value>> {
    sync_proxy(
        &state,
        &format!("/v1/sync/{provider}/link"),
        serde_json::json!({ "user_id": user.user_id, "code": q.code }),
    )
    .await
}

#[derive(Debug, Deserialize, Default, ToSchema)]
pub struct SyncOpts {
    /// `local_wins` | `remote_wins` | `newest_wins`; omitted uses the service default.
    #[serde(default)]
    pub policy: Option<String>,
}

/// Push local state to a provider
///
/// Reflect local watchlist/progress to `provider` (bulk, full-reconciliation walk — see
/// `spawn_targeted_push` for the fast per-series path used automatically when marking a
/// chapter/series read). Response shape is defined by the sync service and forwarded
/// verbatim; not tracked here.
#[utoipa::path(
    post,
    path = "/v1/me/sync/{provider}/push",
    tag = ME_SYNC_TAG,
    params(("provider" = String, Path, description = "Provider slug")),
    request_body(content = Option<SyncOpts>, description = "Optional sync options; omitted body uses the service default"),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Pushed, forwarded from the sync service"),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 409, description = "Account not linked", body = crate::error::ProblemDetails),
    )
)]
pub async fn sync_push(
    State(state): State<AppState>,
    user: AuthUser,
    Path(provider): Path<String>,
    body: Option<Json<SyncOpts>>,
) -> ApiResult<Json<serde_json::Value>> {
    let opts = body.map(|b| b.0).unwrap_or_default();
    sync_proxy(
        &state,
        &format!("/v1/sync/{provider}/push"),
        serde_json::json!({ "user_id": user.user_id, "policy": opts.policy }),
    )
    .await
}

/// Pull a provider's list into local state
///
/// Import `provider`'s list into the local watchlist. Response shape is defined by the sync
/// service and forwarded verbatim; not tracked here.
#[utoipa::path(
    post,
    path = "/v1/me/sync/{provider}/pull",
    tag = ME_SYNC_TAG,
    params(("provider" = String, Path, description = "Provider slug")),
    request_body(content = Option<SyncOpts>, description = "Optional sync options; omitted body uses the service default"),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Pulled, forwarded from the sync service"),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 409, description = "Account not linked", body = crate::error::ProblemDetails),
    )
)]
pub async fn sync_pull(
    State(state): State<AppState>,
    user: AuthUser,
    Path(provider): Path<String>,
    body: Option<Json<SyncOpts>>,
) -> ApiResult<Json<serde_json::Value>> {
    let opts = body.map(|b| b.0).unwrap_or_default();
    sync_proxy(
        &state,
        &format!("/v1/sync/{provider}/pull"),
        serde_json::json!({ "user_id": user.user_id, "policy": opts.policy }),
    )
    .await
}

/// Get automatic-sync settings
///
/// The caller's automatic-sync settings (design v2 §B.6). The body type is shared with the
/// sync service via `tankovault_contracts::sync`.
#[utoipa::path(
    get,
    path = "/v1/me/sync/{provider}/settings",
    tag = ME_SYNC_TAG,
    params(("provider" = String, Path, description = "Provider slug")),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Settings, forwarded from the sync service", body = tankovault_contracts::sync::AccountSettings),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 404, description = "No settings for this provider", body = crate::error::ProblemDetails),
    )
)]
pub async fn sync_settings(
    State(state): State<AppState>,
    user: AuthUser,
    Path(provider): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    sync_get(
        &state,
        &format!("/v1/sync/{provider}/settings/{}", user.user_id.as_uuid()),
    )
    .await
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SyncSettingsPatch {
    #[serde(default)]
    pub auto_sync_enabled: Option<bool>,
    #[serde(default)]
    pub conflict_policy: Option<String>,
}

/// Update automatic-sync settings
///
/// Update automatic sync + conflict policy (design v2 §B.6).
#[utoipa::path(
    patch,
    path = "/v1/me/sync/{provider}/settings",
    tag = ME_SYNC_TAG,
    params(("provider" = String, Path, description = "Provider slug")),
    request_body = SyncSettingsPatch,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Acknowledged", body = serde_json::Value, example = json!({"ok": true})),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn sync_settings_patch(
    State(state): State<AppState>,
    user: AuthUser,
    Path(provider): Path<String>,
    Json(body): Json<SyncSettingsPatch>,
) -> ApiResult<Json<serde_json::Value>> {
    let url = format!(
        "{}/v1/sync/{provider}/settings/{}",
        state.sync_url.trim_end_matches('/'),
        user.user_id.as_uuid()
    );
    let payload = serde_json::json!({
        "user_id": user.user_id,
        "auto_sync_enabled": body.auto_sync_enabled,
        "conflict_policy": body.conflict_policy,
    });
    let resp = state
        .http
        .patch(url)
        .json(&payload)
        .send()
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "sync service unreachable");
            ApiError::Internal
        })?;
    if !resp.status().is_success() {
        return Err(ApiError::Internal);
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// List pending sync conflicts
///
/// The caller's pending conflicts across all providers (§B.6). Rows are `ConflictRow`, the
/// same type the sync service reads from the database.
#[utoipa::path(
    get,
    path = "/v1/me/sync/conflicts",
    tag = ME_SYNC_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Pending conflicts, forwarded from the sync service", body = Vec<tankovault_db::repo::sync::ConflictRow>),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn sync_conflicts(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<serde_json::Value>> {
    sync_get(
        &state,
        &format!("/v1/sync/conflicts/{}", user.user_id.as_uuid()),
    )
    .await
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ResolveConflict {
    pub resolution: String,
}

/// Resolve a sync conflict
///
/// Apply the caller's chosen resolution (§B.6). Response shape is defined by the sync service
/// and forwarded verbatim; not tracked here.
#[utoipa::path(
    post,
    path = "/v1/me/sync/conflicts/{id}/resolve",
    tag = ME_SYNC_TAG,
    params(("id" = uuid::Uuid, Path, description = "Conflict id")),
    request_body = ResolveConflict,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Resolved, forwarded from the sync service"),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 404, description = "No such conflict", body = crate::error::ProblemDetails),
    )
)]
pub async fn sync_resolve_conflict(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<ResolveConflict>,
) -> ApiResult<Json<serde_json::Value>> {
    sync_proxy(
        &state,
        &format!("/v1/sync/conflicts/{id}/resolve"),
        serde_json::json!({ "user_id": user.user_id, "resolution": body.resolution }),
    )
    .await
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct HistoryParams {
    #[serde(default)]
    pub series_id: Option<Uuid>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub page: Option<i64>,
}

/// Get sync history
///
/// A page of the caller's sync history (§B.6). Rows are `HistoryRow`, the same type the sync
/// service reads from the database.
#[utoipa::path(
    get,
    path = "/v1/me/sync/history",
    tag = ME_SYNC_TAG,
    params(HistoryParams),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "A page of sync history, forwarded from the sync service", body = Vec<tankovault_db::repo::sync::HistoryRow>),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn sync_history(
    State(state): State<AppState>,
    user: AuthUser,
    Query(q): Query<HistoryParams>,
) -> ApiResult<Json<serde_json::Value>> {
    let mut path = format!("/v1/sync/history/{}?", user.user_id.as_uuid());
    if let Some(s) = q.series_id {
        let _ = write!(path, "series_id={s}&");
    }
    if let Some(p) = &q.provider {
        let _ = write!(path, "provider={p}&");
    }
    let _ = write!(path, "page={}", q.page.unwrap_or(0));
    sync_get(&state, &path).await
}

/// GET a JSON body from the sync service, mapping upstream errors like `sync_proxy`.
pub(crate) async fn sync_get(state: &AppState, path: &str) -> ApiResult<Json<serde_json::Value>> {
    let url = format!(
        "{}/{}",
        state.sync_url.trim_end_matches('/'),
        path.trim_start_matches('/')
    );
    let resp = state.http.get(url).send().await.map_err(|e| {
        tracing::error!(error = %e, "sync service unreachable");
        ApiError::Internal
    })?;
    if !resp.status().is_success() {
        if resp.status().as_u16() == 404 {
            return Err(ApiError::NotFound);
        }
        return Err(ApiError::Internal);
    }
    Ok(Json(resp.json().await.map_err(|_| ApiError::Internal)?))
}

/// POST a JSON body to the sync service, tolerating an empty (`204`) response and mapping
/// a "not linked" conflict through to the caller. `pub(crate)` so `admin.rs` can reuse it for
/// operator-triggered force pull/push (design: admin Sync console tab).
pub(crate) async fn sync_proxy(
    state: &AppState,
    path: &str,
    body: serde_json::Value,
) -> ApiResult<Json<serde_json::Value>> {
    let url = format!(
        "{}/{}",
        state.sync_url.trim_end_matches('/'),
        path.trim_start_matches('/')
    );
    let resp = state.http.post(url).json(&body).send().await.map_err(|e| {
        tracing::error!(error = %e, "sync service unreachable");
        ApiError::Internal
    })?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        if status.as_u16() == 409 {
            return Err(ApiError::Conflict("Account not linked".to_owned()));
        }
        if status.as_u16() == 404 {
            return Err(ApiError::NotFound);
        }
        tracing::warn!(%status, body = %text, "sync service returned an error");
        return Err(ApiError::Internal);
    }
    let value = if text.trim().is_empty() {
        serde_json::json!({ "ok": true })
    } else {
        serde_json::from_str(&text).unwrap_or_else(|_| serde_json::json!({ "ok": true }))
    };
    Ok(Json(value))
}
