//! Provider CRUD, re-solve, per-provider stats, and the dry-run adapter test.

use super::scans::TriggerScan;
use crate::audit::audit;
use crate::error::{ApiError, ApiResult};
use crate::openapi::ADMIN_PROVIDERS_TAG;
use crate::state::{AppState, AuthUser};
use crate::views::IntoView;
use axum::Json;
use axum::extract::{Path, State};
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;
use tankovault_adapters::{Ctx, SourceAdapter, build_adapter};
use tankovault_db::repo::providers::NewProvider;
use tankovault_domain::{
    AdapterKind, Permission, Politeness, Provider, ProviderId, ProviderState, ScanMode,
};
use tankovault_fetch::{
    HttpChallengeSolver, InMemorySessionStore, ProviderFetchConfig, SessionStore,
    build_provider_fetcher,
};
use tankovault_solver::ChallengeSolver;
use utoipa::ToSchema;

/// List providers
#[utoipa::path(
    get,
    path = "/v1/admin/providers",
    tag = ADMIN_PROVIDERS_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "All providers", body = Vec<Provider>),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
    )
)]
pub async fn list_providers(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<Vec<Provider>>> {
    user.require(Permission::ProvidersRead).await?;
    Ok(Json(
        tankovault_db::repo::providers::list(&state.pool).await?,
    ))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateProvider {
    pub slug: String,
    pub name: String,
    pub base_url: String,
    pub adapter: AdapterKind,
    #[serde(default = "empty_object")]
    pub config: serde_json::Value,
    #[serde(default)]
    pub politeness: Politeness,
}

fn empty_object() -> serde_json::Value {
    serde_json::json!({})
}

/// Refuse a `base_url` that points anywhere the crawler must not go.
///
/// This is the *supplying* side of the SSRF guard, and it was missing: a stored provider row
/// is not a one-off fetch. `ProvidersCreate` + `ProvidersTest` — a role well short of full
/// admin — could point a provider at `http://169.254.169.254` and read the dry-run's parsed
/// output or, on a parse failure, the error text containing it. Worse, the scheduled scan
/// workers then keep hitting the target on a timer, long after the operator's grant is gone.
///
/// Resolution is performed here, not only scheme and IP-literal checks, because the value is
/// being *persisted*: refusing to store a host that resolves internally is cheaper than
/// rediscovering it on every scan.
async fn validate_base_url(base_url: &str) -> ApiResult<()> {
    tankovault_fetch::ssrf::validate_and_resolve(base_url)
        .await
        .map_err(|e| {
            tracing::warn!(url = %base_url, error = %e, "refused a provider base_url");
            ApiError::BadRequest(format!(
                "base_url {base_url:?} is not an allowed target: {e}"
            ))
        })
}

/// Create a provider
#[utoipa::path(
    post,
    path = "/v1/admin/providers",
    tag = ADMIN_PROVIDERS_TAG,
    request_body = CreateProvider,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Created", body = Provider),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
        (status = 400, description = "Provider slug is not a legal bus token", body = crate::error::ProblemDetails),
        (status = 409, description = "Provider slug already exists", body = crate::error::ProblemDetails),
    )
)]
pub async fn create_provider(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<CreateProvider>,
) -> ApiResult<Json<Provider>> {
    user.require(Permission::ProvidersCreate).await?;
    // The slug is not just a label: it becomes the provider's NATS task subject and the
    // name of the worker consumer that drains it. A slug NATS will not accept there yields
    // a provider whose scans are planned, published into nothing, and never run — so it is
    // rejected here rather than discovered as a run that never progresses.
    if !tankovault_contracts::is_valid_provider_slug(&req.slug) {
        return Err(ApiError::BadRequest(format!(
            "provider slug {:?} must be non-empty and contain only letters, digits, '-' or '_'",
            req.slug
        )));
    }
    validate_base_url(&req.base_url).await?;
    let provider = tankovault_db::repo::providers::create(
        &state.pool,
        NewProvider {
            slug: req.slug,
            name: req.name,
            base_url: req.base_url,
            adapter: req.adapter,
            config: req.config,
            politeness: req.politeness,
        },
    )
    .await?;

    audit(
        &state,
        &user,
        "provider.create",
        &provider.id.to_string(),
        &serde_json::json!({
            "slug": provider.slug,
            "base_url": provider.base_url,
        }),
    )
    .await;

    Ok(Json(provider))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateProvider {
    pub name: String,
    pub base_url: String,
    #[serde(default = "empty_object")]
    pub config: serde_json::Value,
    #[serde(default)]
    pub politeness: Politeness,
}

/// Update a provider
///
/// Includes the domain-migration `base_url` change: one field, and every stored relative link
/// re-resolves against the new domain.
#[utoipa::path(
    patch,
    path = "/v1/admin/providers/{id}",
    tag = ADMIN_PROVIDERS_TAG,
    params(("id" = ProviderId, Path, description = "Provider id")),
    request_body = UpdateProvider,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Updated", body = Provider),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
        (status = 404, description = "Provider not found", body = crate::error::ProblemDetails),
    )
)]
pub async fn update_provider(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<ProviderId>,
    Json(req): Json<UpdateProvider>,
) -> ApiResult<Json<Provider>> {
    user.require(Permission::ProvidersWrite).await?;
    let before = tankovault_db::repo::providers::get(&state.pool, id).await?;
    validate_base_url(&req.base_url).await?;
    let provider = tankovault_db::repo::providers::update(
        &state.pool,
        id,
        &req.name,
        &req.base_url,
        &req.config,
        req.politeness,
    )
    .await?;

    let migrated = before.base_url != provider.base_url;
    audit(
        &state,
        &user,
        "provider.update",
        &id.to_string(),
        &serde_json::json!({
            "domain_migration": migrated,
            "base_url_from": before.base_url,
            "base_url_to": provider.base_url,
        }),
    )
    .await;

    Ok(Json(provider))
}

/// Delete a provider
///
/// Remove a provider entirely. Its stored source links cascade-delete (FK `ON DELETE
/// CASCADE`); scan-run history is retained with a nulled provider. Admin-only because it is
/// destructive and irreversible.
#[utoipa::path(
    delete,
    path = "/v1/admin/providers/{id}",
    tag = ADMIN_PROVIDERS_TAG,
    params(("id" = ProviderId, Path, description = "Provider id")),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Acknowledged", body = serde_json::Value, example = json!({"ok": true})),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
        (status = 404, description = "Provider not found", body = crate::error::ProblemDetails),
    )
)]
pub async fn delete_provider(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<ProviderId>,
) -> ApiResult<Json<serde_json::Value>> {
    user.require(Permission::ProvidersDelete).await?;
    let before = tankovault_db::repo::providers::get(&state.pool, id).await?;
    tankovault_db::repo::providers::delete(&state.pool, id).await?;
    audit(
        &state,
        &user,
        "provider.delete",
        &id.to_string(),
        &serde_json::json!({ "slug": before.slug, "base_url": before.base_url }),
    )
    .await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetProviderState {
    pub state: ProviderState,
}

/// Set a provider's health state
///
/// Operator override of a provider's health state: `disabled` pauses all crawling; `active`
/// re-enables it (e.g. clearing a tripped circuit breaker). The scanner/circuit breaker may
/// still transition it afterwards.
#[utoipa::path(
    post,
    path = "/v1/admin/providers/{id}/state",
    tag = ADMIN_PROVIDERS_TAG,
    params(("id" = ProviderId, Path, description = "Provider id")),
    request_body = SetProviderState,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Updated", body = Provider),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
        (status = 404, description = "Provider not found", body = crate::error::ProblemDetails),
    )
)]
pub async fn set_provider_state(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<ProviderId>,
    Json(req): Json<SetProviderState>,
) -> ApiResult<Json<Provider>> {
    user.require(Permission::ProvidersState).await?;
    tankovault_db::repo::providers::set_state(&state.pool, id, req.state).await?;
    let provider = tankovault_db::repo::providers::get(&state.pool, id).await?;
    audit(
        &state,
        &user,
        "provider.set_state",
        &id.to_string(),
        &serde_json::json!({ "state": req.state.as_str() }),
    )
    .await;
    Ok(Json(provider))
}

/// Re-solve a provider
///
/// Re-solve/refresh a single provider by queuing a **fast** re-scan (frontend §9.5). This is
/// the console "Re-solve" action; it is proxied to the control-plane planner exactly like
/// [`trigger_scan`], scoped to one provider.
#[utoipa::path(
    post,
    path = "/v1/admin/providers/{id}/resolve",
    tag = ADMIN_PROVIDERS_TAG,
    params(("id" = ProviderId, Path, description = "Provider id")),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Scan queued, forwarded from the control-plane"),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
        (status = 404, description = "Provider not found", body = crate::error::ProblemDetails),
    )
)]
pub async fn resolve_provider(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<ProviderId>,
) -> ApiResult<Json<serde_json::Value>> {
    user.require(Permission::ProvidersTest).await?;
    // Confirm the provider exists (and surface a clean 404 otherwise) before queuing work.
    let provider = tankovault_db::repo::providers::get(&state.pool, id).await?;

    let req = TriggerScan {
        provider_id: Some(id),
        mode: ScanMode::Fast,
    };
    let Json(body) = state.control_plane.post("/internal/scans", &req).await?;

    audit(
        &state,
        &user,
        "provider.resolve",
        &id.to_string(),
        &serde_json::json!({ "slug": provider.slug, "mode": "fast" }),
    )
    .await;
    Ok(Json(body))
}

/// Get per-provider crawl stats
///
/// Per-provider crawl statistics table.
#[utoipa::path(
    get,
    path = "/v1/admin/providers/stats",
    tag = ADMIN_PROVIDERS_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Per-provider stats", body = Vec<tankovault_contracts::admin::ProviderStatView>),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
    )
)]
pub async fn provider_stats(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<Vec<tankovault_contracts::admin::ProviderStatView>>> {
    user.require(Permission::ProvidersRead).await?;
    let rows = tankovault_db::repo::stats::provider_stats(&state.pool).await?;
    Ok(Json(rows.into_view()))
}

#[derive(Debug, Deserialize, Default, ToSchema)]
pub struct TestAdapterRequest {
    /// Optional relative series path to also fetch metadata + chapters for.
    #[serde(default)]
    pub path: Option<String>,
}

/// Dry-run a provider's adapter
///
/// Dry-run the provider's adapter against the live site and return the parsed sample so
/// operators can fix selectors without a deploy (design §11/§17). Bounded by a timeout;
/// SSRF and rate limits are enforced by the injected fetch stack.
///
/// The body is deliberately free-form JSON: it is a diagnostic dump whose shape follows
/// whatever the adapter managed to parse, and the console renders it verbatim. It is still
/// declared as a schema so the generated client can return it, rather than forcing callers
/// onto an untyped side channel.
#[utoipa::path(
    post,
    path = "/v1/admin/providers/{id}/test",
    tag = ADMIN_PROVIDERS_TAG,
    params(("id" = ProviderId, Path, description = "Provider id")),
    request_body(content = Option<TestAdapterRequest>, description = "Optional relative series path to also fetch metadata + chapters for"),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Dry-run sample (adapter list/fetch results, each individually ok/error)", body = serde_json::Value, example = json!({"provider": "kunmanga", "latest": {"ok": true, "items": []}})),
        (status = 400, description = "Adapter build failed or the dry-run timed out", body = crate::error::ProblemDetails),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "caller does not hold the required permission", body = crate::error::ProblemDetails),
        (status = 404, description = "Provider not found", body = crate::error::ProblemDetails),
    )
)]
pub async fn test_adapter(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<ProviderId>,
    body: Option<Json<TestAdapterRequest>>,
) -> ApiResult<Json<serde_json::Value>> {
    user.require(Permission::ProvidersTest).await?;
    let req = body.map(|b| b.0).unwrap_or_default();
    let provider = tankovault_db::repo::providers::get(&state.pool, id).await?;
    let (adapter, ctx) = build_test_context(
        &provider,
        &state.challenge_solver_url,
        state.internal_token.as_ref(),
    )?;

    let sample = tokio::time::timeout(Duration::from_secs(25), async {
        let latest = match adapter.list_latest(&ctx).await {
            Ok(items) => serde_json::json!({
                "ok": true,
                "items": items.iter().take(10).map(|u| serde_json::json!({
                    "path": u.path, "title": u.title, "latest_chapter": u.latest_chapter,
                })).collect::<Vec<_>>(),
            }),
            Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
        };
        let series = req.path.as_deref().map(|path| async move {
            let meta = match adapter.fetch_series(&ctx, path).await {
                Ok(m) => serde_json::json!({
                    "ok": true, "title": m.title, "alt_titles": m.alt_titles,
                    "description": m.description, "cover_url": m.cover_url, "tags": m.tags,
                    "status": m.status.as_str(), "content_type": m.content_type.as_str(),
                }),
                Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
            };
            let chapters = match adapter.fetch_chapters(&ctx, path).await {
                Ok(list) => serde_json::json!({
                    "ok": true, "count": list.len(),
                    "sample": list.iter().take(10).map(|c| serde_json::json!({
                        "number": c.number, "title": c.title, "path": c.path,
                    })).collect::<Vec<_>>(),
                }),
                Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
            };
            serde_json::json!({ "meta": meta, "chapters": chapters })
        });
        let series = match series {
            Some(fut) => Some(fut.await),
            None => None,
        };
        serde_json::json!({
            "provider": provider.slug,
            "base_url": provider.base_url,
            "latest": latest,
            "series": series,
        })
    })
    .await
    .map_err(|_| ApiError::BadRequest("adapter test timed out".to_owned()))?;

    audit(
        &state,
        &user,
        "provider.test",
        &id.to_string(),
        &serde_json::json!({ "path": req.path }),
    )
    .await;
    Ok(Json(sample))
}

/// Build the provider's adapter + an injected fetch stack for a one-off dry-run. Mirrors
/// the worker's per-provider context; kept inline to avoid a shared crate for one endpoint.
fn build_test_context(
    provider: &Provider,
    solver_url: &str,
    token: Option<&tankovault_service::InternalToken>,
) -> ApiResult<(Box<dyn SourceAdapter>, Ctx)> {
    let adapter = build_adapter(provider.adapter, &provider.slug, &provider.config)
        .map_err(|e| ApiError::BadRequest(format!("adapter build failed: {e}")))?;
    let solver: Arc<dyn ChallengeSolver> = Arc::new(HttpChallengeSolver::new(
        solver_url.to_owned(),
        Duration::from_secs(90),
        token.map(|t| t.expose().to_owned()),
    ));
    let session_store: Arc<dyn SessionStore> = Arc::new(InMemorySessionStore::default());
    let mut cfg = ProviderFetchConfig::new(
        provider.politeness.user_agent.clone(),
        solver,
        session_store,
    );
    cfg.emulation = provider.politeness.emulation;
    cfg.rps = provider.politeness.rps;
    cfg.concurrency = provider.politeness.concurrency;
    cfg.connect_timeout = Duration::from_secs(10);
    cfg.request_timeout = Duration::from_secs(20);
    let fetcher = build_provider_fetcher(cfg).map_err(|e| {
        tracing::error!(error = %e, "fetcher build failed");
        ApiError::Internal
    })?;
    Ok((
        adapter,
        Ctx {
            base_url: provider.base_url.clone(),
            provider_slug: provider.slug.clone(),
            fetcher,
        },
    ))
}
