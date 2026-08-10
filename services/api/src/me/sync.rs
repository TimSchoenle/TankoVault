//! External-sync proxy endpoints, keyed by provider.

use crate::error::ApiResult;
use crate::openapi::ME_SYNC_TAG;
use crate::slug::ProviderSlug;
use crate::state::{AppState, AuthUser};
use crate::step_up::Elevated;
use axum::Json;
use axum::extract::{Path, Query, State};
use serde::Deserialize;
use std::fmt::Write as _;
use tankovault_contracts::sync::{
    AccountSettings, AccountStatus, Ack, AuthorizeUrl, ConflictPolicy, ConflictView, HistoryView,
    ProviderInfo, PullReport, PushReport, Removed, Resolved,
};
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
) -> ApiResult<Json<Vec<ProviderInfo>>> {
    state.sync.get("/v1/sync/providers").await
}

/// Get a provider's OAuth consent URL
///
/// Returns the provider's consent URL (proxied). The body type is shared with the sync
/// service via `tankovault_contracts::sync`.
///
/// Behind a step-up: following this URL grants a third party a standing OAuth token against the
/// caller's account there, which is an authorisation that outlives the session and is not
/// revoked by anything this system does.
#[utoipa::path(
    get,
    path = "/v1/me/sync/{provider}/authorize",
    tag = ME_SYNC_TAG,
    params(("provider" = String, Path, description = "Provider slug")),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Consent URL, forwarded from the sync service", body = tankovault_contracts::sync::AuthorizeUrl),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "a step-up is required", body = crate::error::ProblemDetails),
    )
)]
pub async fn sync_authorize_url(
    State(state): State<AppState>,
    Elevated(_user): Elevated,
    Path(provider): Path<ProviderSlug>,
) -> ApiResult<Json<AuthorizeUrl>> {
    state
        .sync
        .get(&format!("/v1/sync/{provider}/authorize-url"))
        .await
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
    Path(provider): Path<ProviderSlug>,
) -> ApiResult<Json<AccountStatus>> {
    state
        .sync
        .get(&format!(
            "/v1/sync/{provider}/status/{}",
            user.user_id.as_uuid()
        ))
        .await
}

/// Unlink a provider
///
/// Unlink the caller's account at `provider`.
#[utoipa::path(
    delete,
    path = "/v1/me/sync/{provider}",
    tag = ME_SYNC_TAG,
    params(("provider" = String, Path, description = "Provider slug")),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Whether a link was removed", body = Removed),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "a step-up is required", body = crate::error::ProblemDetails),
    )
)]
pub async fn sync_disconnect(
    State(state): State<AppState>,
    Elevated(user): Elevated,
    Path(provider): Path<ProviderSlug>,
) -> ApiResult<Json<Removed>> {
    state
        .sync
        .delete(
            &format!("/v1/sync/{provider}/link"),
            &serde_json::json!({ "user_id": user.user_id }),
        )
        .await
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct AniListCallback {
    pub code: String,
}

/// Complete an OAuth link
///
/// Exchanges the authorization `code` and links the caller's account at `provider`. The sync
/// service answers `204`; [`Ack`] is what `Upstream::decode` synthesises from an empty body.
#[utoipa::path(
    get,
    path = "/v1/me/sync/{provider}/callback",
    tag = ME_SYNC_TAG,
    params(("provider" = String, Path, description = "Provider slug"), AniListCallback),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Linked", body = Ack),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 404, description = "Unknown provider", body = crate::error::ProblemDetails),
        (status = 409, description = "Account not linked", body = crate::error::ProblemDetails),
    )
)]
pub async fn sync_callback(
    State(state): State<AppState>,
    user: AuthUser,
    Path(provider): Path<ProviderSlug>,
    Query(q): Query<AniListCallback>,
) -> ApiResult<Json<Ack>> {
    sync_proxy(
        &state,
        &format!("/v1/sync/{provider}/link"),
        serde_json::json!({ "user_id": user.user_id, "code": q.code }),
    )
    .await
}

#[derive(Debug, Deserialize, Default, ToSchema)]
pub struct SyncOpts {
    /// Overrides the account's own conflict policy for this run; omitted uses it.
    ///
    /// Typed rather than a bare string since FRONTEND F10: the sync service's request body
    /// has always been a `ConflictPolicy`, so a token this proxy accepted and that one did not
    /// used to fail one hop downstream, as a `502` with no useful detail. Now the two name the
    /// same type and the rejection happens here, as a `422` that says which values are legal.
    #[serde(default)]
    pub policy: Option<ConflictPolicy>,
}

/// Push local state to a provider
///
/// Reflect local watchlist/progress to `provider` (bulk, full-reconciliation walk — see
/// `spawn_targeted_push` for the fast per-series path used automatically when marking a
/// chapter/series read).
#[utoipa::path(
    post,
    path = "/v1/me/sync/{provider}/push",
    tag = ME_SYNC_TAG,
    params(("provider" = String, Path, description = "Provider slug")),
    request_body(content = Option<SyncOpts>, description = "Optional sync options; omitted body uses the service default"),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "What the push considered and wrote", body = PushReport),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 409, description = "Account not linked", body = crate::error::ProblemDetails),
    )
)]
pub async fn sync_push(
    State(state): State<AppState>,
    user: AuthUser,
    Path(provider): Path<ProviderSlug>,
    body: Option<Json<SyncOpts>>,
) -> ApiResult<Json<PushReport>> {
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
/// Import `provider`'s list into the local watchlist.
#[utoipa::path(
    post,
    path = "/v1/me/sync/{provider}/pull",
    tag = ME_SYNC_TAG,
    params(("provider" = String, Path, description = "Provider slug")),
    request_body(content = Option<SyncOpts>, description = "Optional sync options; omitted body uses the service default"),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "What the pull fetched, matched and wrote", body = PullReport),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 409, description = "Account not linked", body = crate::error::ProblemDetails),
    )
)]
pub async fn sync_pull(
    State(state): State<AppState>,
    user: AuthUser,
    Path(provider): Path<ProviderSlug>,
    body: Option<Json<SyncOpts>>,
) -> ApiResult<Json<PullReport>> {
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
    Path(provider): Path<ProviderSlug>,
) -> ApiResult<Json<AccountSettings>> {
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
    /// The account's new conflict policy; omitted leaves it unchanged.
    ///
    /// See [`SyncOpts::policy`] for why this is typed rather than a bare string.
    #[serde(default)]
    pub conflict_policy: Option<ConflictPolicy>,
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
        (status = 200, description = "Acknowledged", body = Ack),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn sync_settings_patch(
    State(state): State<AppState>,
    user: AuthUser,
    Path(provider): Path<ProviderSlug>,
    Json(body): Json<SyncSettingsPatch>,
) -> ApiResult<Json<Ack>> {
    let payload = serde_json::json!({
        "user_id": user.user_id,
        "auto_sync_enabled": body.auto_sync_enabled,
        "conflict_policy": body.conflict_policy,
    });
    // The sync service answers `204`; forwarding what `Upstream::decode` synthesises keeps one
    // source for this body rather than re-stating it as a literal here.
    state
        .sync
        .patch(
            &format!("/v1/sync/{provider}/settings/{}", user.user_id.as_uuid()),
            &payload,
        )
        .await
}

/// List pending sync conflicts
///
/// The caller's pending conflicts across all providers (§B.6). Rows are
/// `tankovault_contracts::sync::ConflictView`, the shape the sync service publishes.
#[utoipa::path(
    get,
    path = "/v1/me/sync/conflicts",
    tag = ME_SYNC_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Pending conflicts, forwarded from the sync service", body = Vec<tankovault_contracts::sync::ConflictView>),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn sync_conflicts(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<Vec<ConflictView>>> {
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
/// Apply the caller's chosen resolution (§B.6).
#[utoipa::path(
    post,
    path = "/v1/me/sync/conflicts/{id}/resolve",
    tag = ME_SYNC_TAG,
    params(("id" = uuid::Uuid, Path, description = "Conflict id")),
    request_body = ResolveConflict,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Whether a conflict was settled", body = Resolved),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 404, description = "No such conflict", body = crate::error::ProblemDetails),
    )
)]
pub async fn sync_resolve_conflict(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<ResolveConflict>,
) -> ApiResult<Json<Resolved>> {
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
    // Plain `//`, not `///` — a doc comment here would move the published parameter
    // description via `value_type = String` (rule 9). See `sync_history` for why this field
    // is `ProviderSlug`, not `String`.
    #[param(value_type = String)]
    #[serde(default)]
    pub provider: Option<ProviderSlug>,
    #[serde(default)]
    pub page: Option<i64>,
}

/// Get sync history
///
/// A page of the caller's sync history (§B.6). Rows are
/// `tankovault_contracts::sync::HistoryView`, the shape the sync service publishes.
#[utoipa::path(
    get,
    path = "/v1/me/sync/history",
    tag = ME_SYNC_TAG,
    params(HistoryParams),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "A page of sync history, forwarded from the sync service", body = Vec<tankovault_contracts::sync::HistoryView>),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn sync_history(
    State(state): State<AppState>,
    user: AuthUser,
    Query(q): Query<HistoryParams>,
) -> ApiResult<Json<Vec<HistoryView>>> {
    let mut path = format!("/v1/sync/history/{}?", user.user_id.as_uuid());
    if let Some(s) = q.series_id {
        let _ = write!(path, "series_id={s}&");
    }
    if let Some(p) = &q.provider {
        // No percent-encoding needed: `ProviderSlug` is `[A-Za-z0-9_-]+`, already URL-safe.
        // Widening it back to `String` would re-open query injection via `&`, `=`, `#`.
        let _ = write!(path, "provider={p}&");
    }
    let _ = write!(path, "page={}", q.page.unwrap_or(0));
    sync_get(&state, &path).await
}

/// GET a JSON body from the sync service; a thin alias over [`crate::upstream::Upstream`] so
/// `admin/sync.rs` reads the same way as this module.
pub(crate) async fn sync_get<T: serde::de::DeserializeOwned>(
    state: &AppState,
    path: &str,
) -> ApiResult<Json<T>> {
    state.sync.get(path).await
}

/// POST a JSON body to the sync service. `pub(crate)` so `admin/sync.rs` can reuse it for
/// operator-triggered force pull/push.
///
/// Generic in the response so each caller names the body it publishes; that name is what makes
/// the endpoint's `#[utoipa::path]` declaration true, since `Upstream::decode` fails the request
/// when the peer answers with something else.
pub(crate) async fn sync_proxy<T: serde::de::DeserializeOwned>(
    state: &AppState,
    path: &str,
    body: serde_json::Value,
) -> ApiResult<Json<T>> {
    state.sync.post(path, &body).await
}
