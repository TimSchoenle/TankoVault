//! Operator visibility and manual intervention for external-sync accounts and mappings.

use crate::audit::audit;
use crate::error::{ApiError, ApiResult};
use crate::openapi::ADMIN_SYNC_TAG;
use crate::state::{AppState, AuthUser};
use axum::Json;
use axum::extract::{Path, State};
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use tankovault_domain::{Permission, SeriesId, UserId, WatchStatus};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// External sync — admin visibility + operator actions (design: admin Sync console tab)
// ---------------------------------------------------------------------------

/// List linked external accounts
///
/// Every linked external account across all users.
#[utoipa::path(
    get,
    path = "/v1/admin/sync/accounts",
    tag = ADMIN_SYNC_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Up to 200 linked accounts", body = Vec<tankovault_db::repo::sync::AdminAccountRow>),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
    )
)]
pub async fn list_sync_accounts(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<Vec<tankovault_db::repo::sync::AdminAccountRow>>> {
    user.require(Permission::SyncAdminRead).await?;
    Ok(Json(
        tankovault_db::repo::sync::admin_list_accounts(&state.pool, 200).await?,
    ))
}

/// List series↔external mappings
///
/// Every series↔external mapping across all providers.
#[utoipa::path(
    get,
    path = "/v1/admin/sync/mappings",
    tag = ADMIN_SYNC_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Up to 200 mappings", body = Vec<tankovault_db::repo::sync::AdminMappingRow>),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
    )
)]
pub async fn list_sync_mappings(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<Vec<tankovault_db::repo::sync::AdminMappingRow>>> {
    user.require(Permission::SyncAdminRead).await?;
    Ok(Json(
        tankovault_db::repo::sync::admin_list_mappings(&state.pool, 200).await?,
    ))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SyncAccountTarget {
    pub user_id: UserId,
    pub provider: String,
}

/// Force-pull another user's linked account
///
/// Operator-forced pull for another user's linked account. Response shape is defined by the
/// sync service and forwarded verbatim; not tracked here.
#[utoipa::path(
    post,
    path = "/v1/admin/sync/pull",
    tag = ADMIN_SYNC_TAG,
    request_body = SyncAccountTarget,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Pulled, forwarded from the sync service"),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
        (status = 409, description = "Account not linked", body = crate::error::ProblemDetails),
    )
)]
pub async fn admin_sync_pull(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<SyncAccountTarget>,
) -> ApiResult<Json<serde_json::Value>> {
    user.require(Permission::SyncAdminWrite).await?;
    let body = crate::me::sync_proxy(
        &state,
        &format!("/v1/sync/{}/pull", req.provider),
        serde_json::json!({ "user_id": req.user_id }),
    )
    .await?;
    audit(
        &state,
        &user,
        "sync.pull",
        &format!("{}:{}", req.provider, req.user_id.as_uuid()),
        &serde_json::json!({}),
    )
    .await;
    Ok(body)
}

/// Force-push another user's linked account
///
/// Operator-forced push for another user's linked account. Response shape is defined by the
/// sync service and forwarded verbatim; not tracked here.
#[utoipa::path(
    post,
    path = "/v1/admin/sync/push",
    tag = ADMIN_SYNC_TAG,
    request_body = SyncAccountTarget,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Pushed, forwarded from the sync service"),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
        (status = 409, description = "Account not linked", body = crate::error::ProblemDetails),
    )
)]
pub async fn admin_sync_push(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<SyncAccountTarget>,
) -> ApiResult<Json<serde_json::Value>> {
    user.require(Permission::SyncAdminWrite).await?;
    let body = crate::me::sync_proxy(
        &state,
        &format!("/v1/sync/{}/push", req.provider),
        serde_json::json!({ "user_id": req.user_id }),
    )
    .await?;
    audit(
        &state,
        &user,
        "sync.push",
        &format!("{}:{}", req.provider, req.user_id.as_uuid()),
        &serde_json::json!({}),
    )
    .await;
    Ok(body)
}

/// Force-unlink another user's account
///
/// Operator-forced unlink of another user's linked account. Response shape is defined by the
/// sync service and forwarded verbatim; not tracked here.
#[utoipa::path(
    post,
    path = "/v1/admin/sync/unlink",
    tag = ADMIN_SYNC_TAG,
    request_body = SyncAccountTarget,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Unlinked, forwarded from the sync service"),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
    )
)]
pub async fn admin_sync_unlink(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<SyncAccountTarget>,
) -> ApiResult<Json<serde_json::Value>> {
    user.require(Permission::SyncAdminWrite).await?;
    let Json(value) = state
        .sync
        .delete(
            &format!("/v1/sync/{}/link", req.provider),
            &serde_json::json!({ "user_id": req.user_id }),
        )
        .await?;
    audit(
        &state,
        &user,
        "sync.unlink",
        &format!("{}:{}", req.provider, req.user_id.as_uuid()),
        &value,
    )
    .await;
    Ok(Json(value))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SyncMappingTarget {
    pub series_id: SeriesId,
    pub provider: String,
}

/// Clear a series↔external mapping
///
/// Remove a bad series↔external mapping; the next pull/push (or targeted push) re-resolves it
/// from scratch.
#[utoipa::path(
    post,
    path = "/v1/admin/sync/mappings/clear",
    tag = ADMIN_SYNC_TAG,
    request_body = SyncMappingTarget,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Whether a mapping was actually removed", body = serde_json::Value, example = json!({"removed": true})),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
    )
)]
pub async fn clear_sync_mapping(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<SyncMappingTarget>,
) -> ApiResult<Json<serde_json::Value>> {
    user.require(Permission::SyncAdminWrite).await?;
    let removed =
        tankovault_db::repo::sync::delete_mapping(&state.pool, req.series_id, &req.provider)
            .await?;
    audit(
        &state,
        &user,
        "sync.mapping.clear",
        &format!("{}:{}", req.provider, req.series_id.as_uuid()),
        &serde_json::json!({ "removed": removed }),
    )
    .await;
    Ok(Json(serde_json::json!({ "removed": removed })))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpsertMapping {
    pub series_id: SeriesId,
    pub provider: String,
    pub external_id: String,
}

/// Create or correct a series↔external mapping
///
/// Manually create or correct a series↔external mapping (design: admin Sync console tab).
/// Lets an operator fix a wrong external id or add a missing one by hand from the per-series
/// "manga info" editor and the assign queue.
#[utoipa::path(
    post,
    path = "/v1/admin/sync/mappings",
    tag = ADMIN_SYNC_TAG,
    request_body = UpsertMapping,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Acknowledged", body = serde_json::Value, example = json!({"ok": true})),
        (status = 400, description = "provider or external_id is empty", body = crate::error::ProblemDetails),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
    )
)]
pub async fn upsert_sync_mapping(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<UpsertMapping>,
) -> ApiResult<Json<serde_json::Value>> {
    user.require(Permission::SyncAdminWrite).await?;
    let provider = req.provider.trim();
    let external_id = req.external_id.trim();
    if provider.is_empty() || external_id.is_empty() {
        return Err(ApiError::BadRequest(
            "provider and external_id are required".to_owned(),
        ));
    }
    tankovault_db::repo::sync::upsert_mapping(&state.pool, req.series_id, provider, external_id)
        .await?;
    audit(
        &state,
        &user,
        "sync.mapping.upsert",
        &format!("{}:{}", provider, req.series_id.as_uuid()),
        &serde_json::json!({ "external_id": external_id }),
    )
    .await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// List a series' external mappings
///
/// Every external mapping recorded for one series, so the console can render a per-series
/// "manga info" panel showing what it is (and is not) synced to.
#[utoipa::path(
    get,
    path = "/v1/admin/sync/series/{id}",
    tag = ADMIN_SYNC_TAG,
    params(("id" = SeriesId, Path, description = "Series id")),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Mappings for this series", body = Vec<tankovault_db::repo::sync::AdminMappingRow>),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
    )
)]
pub async fn list_sync_mappings_for_series(
    State(state): State<AppState>,
    user: AuthUser,
    Path(series_id): Path<SeriesId>,
) -> ApiResult<Json<Vec<tankovault_db::repo::sync::AdminMappingRow>>> {
    user.require(Permission::SyncAdminRead).await?;
    Ok(Json(
        tankovault_db::repo::sync::admin_list_mappings_for_series(&state.pool, series_id).await?,
    ))
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct UnmappedQuery {
    /// External provider to check membership against (e.g. `anilist`).
    pub provider: String,
    /// Optional case-insensitive title filter.
    #[serde(default)]
    pub query: Option<String>,
}

/// List series without a mapping
///
/// The assign queue: canonical series without a mapping for the given provider, richest
/// first, so operators can review and hand-assign the ones the automatic matcher was not
/// confident enough to link.
#[utoipa::path(
    get,
    path = "/v1/admin/sync/unmapped",
    tag = ADMIN_SYNC_TAG,
    params(UnmappedQuery),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Up to 100 unmapped series", body = Vec<tankovault_db::repo::sync::UnmappedSeriesRow>),
        (status = 400, description = "provider is empty", body = crate::error::ProblemDetails),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
    )
)]
pub async fn list_unmapped_series(
    State(state): State<AppState>,
    user: AuthUser,
    axum::extract::Query(q): axum::extract::Query<UnmappedQuery>,
) -> ApiResult<Json<Vec<tankovault_db::repo::sync::UnmappedSeriesRow>>> {
    user.require(Permission::SyncAdminRead).await?;
    let provider = q.provider.trim();
    if provider.is_empty() {
        return Err(ApiError::BadRequest("provider is required".to_owned()));
    }
    Ok(Json(
        tankovault_db::repo::sync::admin_list_unmapped(
            &state.pool,
            provider,
            q.query.as_deref(),
            100,
        )
        .await?,
    ))
}

/// List unmatched remote entries
///
/// The reverse assign queue: remote provider entries a pull fetched but the auto-matcher
/// could not confidently link to a local series, so an operator can reconcile **every**
/// loaded entry by hand (not just the confident matches).
#[utoipa::path(
    get,
    path = "/v1/admin/sync/unmatched",
    tag = ADMIN_SYNC_TAG,
    params(UnmappedQuery),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Up to 200 unmatched remote entries", body = Vec<tankovault_db::repo::sync::RemoteEntryRow>),
        (status = 400, description = "provider is empty", body = crate::error::ProblemDetails),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
    )
)]
pub async fn list_unmatched_remote(
    State(state): State<AppState>,
    user: AuthUser,
    axum::extract::Query(q): axum::extract::Query<UnmappedQuery>,
) -> ApiResult<Json<Vec<tankovault_db::repo::sync::RemoteEntryRow>>> {
    user.require(Permission::SyncAdminRead).await?;
    let provider = q.provider.trim();
    if provider.is_empty() {
        return Err(ApiError::BadRequest("provider is required".to_owned()));
    }
    Ok(Json(
        tankovault_db::repo::sync::admin_list_unmatched_remote(
            &state.pool,
            provider,
            q.query.as_deref(),
            200,
        )
        .await?,
    ))
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SuggestQuery {
    /// The remote entry's title to match against the local catalogue.
    pub title: String,
    /// Optional local content-type token (`manga`/`manhwa`/…) to sharpen scoring.
    #[serde(default)]
    pub content_type: Option<String>,
    /// Optional release year to sharpen scoring.
    #[serde(default)]
    pub start_year: Option<i32>,
}

/// One ranked suggestion for the admin "match every loaded entry" screen: a local series the
/// matcher thinks the remote entry could be, with enough info (title, type, sources) to
/// eyeball it and its confidence `score` in `[0,1]`.
#[derive(Debug, Serialize, ToSchema)]
pub struct SuggestedMatch {
    pub series_id: Uuid,
    pub title: String,
    pub content_type: String,
    pub release_year: Option<i32>,
    pub source_count: i64,
    pub score: f32,
}

/// Suggest local matches for a remote entry
///
/// Rank local catalogue series as likely matches for a fetched remote entry, so the operator
/// gets automatic suggestions instead of blind-searching. Uses the same trigram candidates as
/// auto-matching but returns the *full* ranked list (with scores) rather than only confident
/// ones, so even weak-but-plausible matches are offered.
#[utoipa::path(
    get,
    path = "/v1/admin/sync/suggest",
    tag = ADMIN_SYNC_TAG,
    params(SuggestQuery),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Up to 8 ranked suggestions, best score first", body = Vec<SuggestedMatch>),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
    )
)]
pub async fn list_suggestions(
    State(state): State<AppState>,
    user: AuthUser,
    axum::extract::Query(q): axum::extract::Query<SuggestQuery>,
) -> ApiResult<Json<Vec<SuggestedMatch>>> {
    user.require(Permission::SyncAdminRead).await?;
    let normalized = tankovault_domain::normalize_title(&q.title);
    if normalized.is_empty() {
        return Ok(Json(Vec::new()));
    }
    let content_type = q
        .content_type
        .as_deref()
        .and_then(|c| tankovault_domain::ContentType::from_str(c).ok())
        .unwrap_or(tankovault_domain::ContentType::Unknown);

    let rows =
        tankovault_db::repo::sync::suggest_series_candidates(&state.pool, &normalized, 25).await?;
    let query = tankovault_matcher::Query {
        normalized_title: normalized,
        content_type,
        release_year: q.start_year,
        // No tag/author signal from this query shape yet — an operator is eyeballing the
        // ranked list anyway, so this stays title/type/year-only for now.
        tags: Vec::new(),
        authors: Vec::new(),
    };
    let mut out: Vec<SuggestedMatch> = rows
        .into_iter()
        .map(|r| {
            let ct = tankovault_domain::ContentType::from_str(&r.content_type)
                .unwrap_or(tankovault_domain::ContentType::Unknown);
            let candidate = tankovault_matcher::Candidate {
                series_id: SeriesId::from_uuid(r.series_id),
                normalized_title: r.normalized_title,
                similarity: r.similarity,
                content_type: ct,
                release_year: r.release_year,
                tags: Vec::new(),
                authors: Vec::new(),
            };
            let score = tankovault_matcher::score(&query, &candidate);
            SuggestedMatch {
                series_id: r.series_id,
                title: r.title,
                content_type: r.content_type,
                release_year: r.release_year,
                source_count: r.source_count,
                score,
            }
        })
        .collect();
    // Best score first; the matcher can reorder relative to raw trigram similarity.
    out.sort_by(|a, b| b.score.total_cmp(&a.score));
    out.truncate(8);
    Ok(Json(out))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct AssignRemoteEntry {
    pub user_id: UserId,
    pub provider: String,
    pub external_id: String,
    pub series_id: SeriesId,
}

/// Assign a remote entry to a local series
///
/// Hand-assign a fetched remote entry to a local series. It records the mapping, imports the
/// entry onto the user's watchlist (status + progress from the stored snapshot) so the result
/// shows immediately, and clears it from the unmatched queue — no fresh pull required.
#[utoipa::path(
    post,
    path = "/v1/admin/sync/assign",
    tag = ADMIN_SYNC_TAG,
    request_body = AssignRemoteEntry,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Acknowledged", body = serde_json::Value, example = json!({"ok": true})),
        (status = 400, description = "Missing provider/external_id, no such remote entry, or the stored entry has an invalid status", body = crate::error::ProblemDetails),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
    )
)]
pub async fn assign_remote_entry(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<AssignRemoteEntry>,
) -> ApiResult<Json<serde_json::Value>> {
    user.require(Permission::SyncAdminWrite).await?;
    let provider = req.provider.trim();
    let external_id = req.external_id.trim();
    if provider.is_empty() || external_id.is_empty() {
        return Err(ApiError::BadRequest(
            "provider and external_id are required".to_owned(),
        ));
    }

    let snapshot = tankovault_db::repo::sync::get_remote_entry(
        &state.pool,
        req.user_id,
        provider,
        external_id,
    )
    .await?
    .ok_or_else(|| ApiError::BadRequest("no such remote entry".to_owned()))?;
    let status = WatchStatus::from_str(&snapshot.status)
        .map_err(|_| ApiError::BadRequest("stored entry has an invalid status".to_owned()))?;

    tankovault_db::repo::sync::upsert_mapping(&state.pool, req.series_id, provider, external_id)
        .await?;
    tankovault_db::repo::tracking::watchlist_set_status(
        &state.pool,
        req.user_id,
        req.series_id,
        status,
    )
    .await?;
    tankovault_db::repo::tracking::progress_set(
        &state.pool,
        req.user_id,
        req.series_id,
        snapshot.progress,
    )
    .await?;
    tankovault_db::repo::sync::mark_remote_entry_matched(
        &state.pool,
        req.user_id,
        provider,
        external_id,
        req.series_id,
    )
    .await?;

    audit(
        &state,
        &user,
        "sync.remote.assign",
        &format!("{provider}:{external_id}"),
        &serde_json::json!({
            "series_id": req.series_id.as_uuid(),
            "user_id": req.user_id.as_uuid(),
        }),
    )
    .await;
    Ok(Json(serde_json::json!({ "ok": true })))
}
