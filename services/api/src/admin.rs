//! Operator/admin handlers (RBAC-gated): provider CRUD (incl. the domain-migration
//! `base_url` edit), scan triggering (proxied to the control-plane), and run status.
//! Every mutating action writes a structured audit record (design §16).

use crate::error::{ApiError, ApiResult};
use crate::state::{AppState, AuthUser};
use axum::Json;
use axum::extract::{Path, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;
use tankovault_adapters::{Ctx, SourceAdapter, build_adapter};
use tankovault_db::repo::matching::MergeCandidateView;
use tankovault_db::repo::providers::NewProvider;
use std::str::FromStr;
use tankovault_domain::{
    AdapterKind, Politeness, Provider, ProviderId, ProviderState, ScanMode, ScanRun, ScanRunId,
    SeriesId, UserId, UserRole, WatchStatus,
};
use tankovault_fetch::{
    HttpChallengeSolver, InMemorySessionStore, ProviderFetchConfig, RobotsRules, SessionStore,
    build_provider_fetcher,
};
use tankovault_solver::ChallengeSolver;
use tokio_stream::StreamExt as _;
use tokio_stream::wrappers::IntervalStream;
use uuid::Uuid;

/// `GET /v1/admin/providers`
pub async fn list_providers(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<Vec<Provider>>> {
    user.require(UserRole::Operator)?;
    Ok(Json(tankovault_db::repo::providers::list(&state.pool).await?))
}

#[derive(Debug, Deserialize)]
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

/// `POST /v1/admin/providers`
pub async fn create_provider(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<CreateProvider>,
) -> ApiResult<Json<Provider>> {
    user.require(UserRole::Admin)?;
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

#[derive(Debug, Deserialize)]
pub struct UpdateProvider {
    pub name: String,
    pub base_url: String,
    #[serde(default = "empty_object")]
    pub config: serde_json::Value,
    #[serde(default)]
    pub politeness: Politeness,
}

/// `PATCH /v1/admin/providers/:id` — includes the domain-migration `base_url` change:
/// one field, and every stored relative link re-resolves against the new domain.
pub async fn update_provider(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<ProviderId>,
    Json(req): Json<UpdateProvider>,
) -> ApiResult<Json<Provider>> {
    user.require(UserRole::Operator)?;
    let before = tankovault_db::repo::providers::get(&state.pool, id).await?;
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

/// `DELETE /v1/admin/providers/:id` — remove a provider entirely. Its stored source links
/// cascade-delete (FK `ON DELETE CASCADE`); scan-run history is retained with a nulled
/// provider. Admin-only because it is destructive and irreversible.
pub async fn delete_provider(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<ProviderId>,
) -> ApiResult<Json<serde_json::Value>> {
    user.require(UserRole::Admin)?;
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

#[derive(Debug, Deserialize)]
pub struct SetProviderState {
    pub state: ProviderState,
}

/// `POST /v1/admin/providers/:id/state` — operator override of a provider's health state:
/// `disabled` pauses all crawling; `active` re-enables it (e.g. clearing a tripped circuit
/// breaker). The scanner/circuit breaker may still transition it afterwards.
pub async fn set_provider_state(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<ProviderId>,
    Json(req): Json<SetProviderState>,
) -> ApiResult<Json<Provider>> {
    user.require(UserRole::Operator)?;
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

/// `POST /v1/admin/providers/:id/resolve` — re-solve/refresh a single provider by queuing a
/// **fast** re-scan (frontend §9.5). This is the console "Re-solve" action; it is proxied to
/// the control-plane planner exactly like [`trigger_scan`], scoped to one provider.
pub async fn resolve_provider(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<ProviderId>,
) -> ApiResult<Json<serde_json::Value>> {
    user.require(UserRole::Operator)?;
    // Confirm the provider exists (and surface a clean 404 otherwise) before queuing work.
    let provider = tankovault_db::repo::providers::get(&state.pool, id).await?;

    let req = TriggerScan {
        provider_id: Some(id),
        mode: ScanMode::Fast,
    };
    let url = format!(
        "{}/internal/scans",
        state.control_plane_url.trim_end_matches('/')
    );
    let resp = state.http.post(url).json(&req).send().await.map_err(|e| {
        tracing::error!(error = %e, "control-plane unreachable");
        ApiError::Internal
    })?;
    if !resp.status().is_success() {
        return Err(ApiError::Internal);
    }
    let body: serde_json::Value = resp.json().await.map_err(|_| ApiError::Internal)?;

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

/// `GET /v1/admin/users` — the operator Users directory: identity, role, and tracked-series
/// count per user (frontend §9.5 Users tab).
pub async fn list_users(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<Vec<tankovault_db::repo::users::UserRow2>>> {
    user.require(UserRole::Operator)?;
    Ok(Json(
        tankovault_db::repo::users::list_users(&state.pool, 200).await?,
    ))
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TriggerScan {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<ProviderId>,
    pub mode: ScanMode,
}

/// `POST /v1/admin/scans` — proxied to the control-plane planner.
pub async fn trigger_scan(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<TriggerScan>,
) -> ApiResult<Json<serde_json::Value>> {
    user.require(UserRole::Operator)?;

    let url = format!(
        "{}/internal/scans",
        state.control_plane_url.trim_end_matches('/')
    );
    let resp = state.http.post(url).json(&req).send().await.map_err(|e| {
        tracing::error!(error = %e, "control-plane unreachable");
        ApiError::Internal
    })?;

    if !resp.status().is_success() {
        return Err(ApiError::Internal);
    }
    let body: serde_json::Value = resp.json().await.map_err(|_| ApiError::Internal)?;

    audit(
        &state,
        &user,
        "scan.trigger",
        "-",
        &serde_json::to_value(&req).unwrap_or_default(),
    )
    .await;
    Ok(Json(body))
}

/// `GET /v1/admin/scans/:run_id`
pub async fn get_scan(
    State(state): State<AppState>,
    user: AuthUser,
    Path(run_id): Path<ScanRunId>,
) -> ApiResult<Json<ScanRun>> {
    user.require(UserRole::Operator)?;
    Ok(Json(
        tankovault_db::repo::scans::get_run(&state.pool, run_id).await?,
    ))
}

/// `GET /v1/admin/scans` — the most recent scan runs (the console's scan-queue overview).
/// The live variant is `/v1/admin/scans/stream`; this GET gives the console its first paint
/// and drives its polling refresh.
pub async fn list_scans(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<Vec<ScanRun>>> {
    user.require(UserRole::Operator)?;
    Ok(Json(
        tankovault_db::repo::scans::list_recent_runs(&state.pool, 30).await?,
    ))
}

/// `GET /v1/admin/scan-failures` — the most recently failed scan tasks with their errors,
/// for triaging stuck providers / broken selectors (design §17.2.7).
pub async fn scan_failures(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<Vec<tankovault_db::repo::scans::FailedTaskView>>> {
    user.require(UserRole::Operator)?;
    Ok(Json(
        tankovault_db::repo::scans::recent_failed_tasks(&state.pool, 25).await?,
    ))
}

/// `GET /v1/admin/stats` — system-wide rollup for the console header.
pub async fn system_stats(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<tankovault_db::repo::stats::SystemStats>> {
    user.require(UserRole::Operator)?;
    Ok(Json(
        tankovault_db::repo::stats::system_overview(&state.pool).await?,
    ))
}

/// `GET /v1/admin/providers/stats` — per-provider crawl statistics table.
pub async fn provider_stats(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<Vec<tankovault_db::repo::stats::ProviderStat>>> {
    user.require(UserRole::Operator)?;
    Ok(Json(
        tankovault_db::repo::stats::provider_stats(&state.pool).await?,
    ))
}

/// `GET /v1/admin/audit` — the most recent privileged actions (design §16 audit trail).
pub async fn audit_log(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<Vec<tankovault_db::repo::audit::AuditView>>> {
    user.require(UserRole::Operator)?;
    Ok(Json(
        tankovault_db::repo::audit::list_recent(&state.pool, 40).await?,
    ))
}

/// `GET /v1/admin/merge-candidates` — the canonicalisation review queue (design §10).
pub async fn list_merge_candidates(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<Vec<MergeCandidateView>>> {
    user.require(UserRole::Operator)?;
    Ok(Json(
        tankovault_db::repo::matching::list_open_merge_candidates(&state.pool, 200).await?,
    ))
}

#[derive(Debug, Deserialize)]
pub struct MergeRequest {
    /// The surviving canonical series.
    pub keep: SeriesId,
    /// The series merged into `keep` and then deleted.
    pub merge: SeriesId,
}

/// `POST /v1/admin/series/merge` — transactional re-parent + title/tag union (design §10).
pub async fn merge_series(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<MergeRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    user.require(UserRole::Operator)?;
    tankovault_db::repo::matching::merge_series(&state.pool, req.keep, req.merge, Some(user.user_id))
        .await?;
    audit(
        &state,
        &user,
        "series.merge",
        &req.merge.to_string(),
        &serde_json::json!({ "keep": req.keep, "merged": req.merge }),
    )
    .await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Debug, Deserialize)]
pub struct DismissRequest {
    pub id: Uuid,
}

/// `POST /v1/admin/merge-candidates/dismiss` — operator judged the two works distinct.
pub async fn dismiss_merge_candidate(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<DismissRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    user.require(UserRole::Operator)?;
    let dismissed =
        tankovault_db::repo::matching::dismiss_merge_candidate(&state.pool, req.id, Some(user.user_id))
            .await?;
    audit(
        &state,
        &user,
        "merge_candidate.dismiss",
        &req.id.to_string(),
        &serde_json::json!({ "dismissed": dismissed }),
    )
    .await;
    Ok(Json(serde_json::json!({ "dismissed": dismissed })))
}

/// `GET /v1/admin/scans/stream` — SSE live scan progress for the operator console. Polls
/// the durable `scan_runs` (the system of record for progress) every 2 s and pushes a
/// `runs` event; a `scan.progress` NATS relay is a documented enhancement.
pub async fn scan_stream(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>>> {
    user.require(UserRole::Operator)?;
    let pool = state.pool.clone();
    let stream =
        IntervalStream::new(tokio::time::interval(Duration::from_secs(2))).then(move |_| {
            let pool = pool.clone();
            async move {
                let runs = tankovault_db::repo::scans::list_recent_runs(&pool, 20)
                    .await
                    .unwrap_or_default();
                let event = Event::default()
                    .event("runs")
                    .json_data(&runs)
                    .unwrap_or_else(|_| Event::default().comment("serialize error"));
                Ok(event)
            }
        });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

#[derive(Debug, Deserialize, Default)]
pub struct TestAdapterRequest {
    /// Optional relative series path to also fetch metadata + chapters for.
    #[serde(default)]
    pub path: Option<String>,
}

/// `POST /v1/admin/providers/:id/test` — dry-run the provider's adapter against the live
/// site and return the parsed sample so operators can fix selectors without a deploy
/// (design §11/§17). Bounded by a timeout; SSRF/robots/rate-limit are enforced by the
/// injected fetch stack.
pub async fn test_adapter(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<ProviderId>,
    body: Option<Json<TestAdapterRequest>>,
) -> ApiResult<Json<serde_json::Value>> {
    user.require(UserRole::Operator)?;
    let req = body.map(|b| b.0).unwrap_or_default();
    let provider = tankovault_db::repo::providers::get(&state.pool, id).await?;
    let (adapter, ctx) = build_test_context(&provider, &state.challenge_solver_url)?;

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
) -> ApiResult<(Box<dyn SourceAdapter>, Ctx)> {
    let adapter = build_adapter(provider.adapter, &provider.slug, &provider.config)
        .map_err(|e| ApiError::BadRequest(format!("adapter build failed: {e}")))?;
    let robots = provider
        .robots_txt
        .as_deref()
        .map(|txt| RobotsRules::parse(txt, &provider.politeness.user_agent));
    let solver: Arc<dyn ChallengeSolver> = Arc::new(HttpChallengeSolver::new(
        solver_url.to_owned(),
        Duration::from_secs(90),
    ));
    let session_store: Arc<dyn SessionStore> = Arc::new(InMemorySessionStore::default());
    let mut cfg = ProviderFetchConfig::new(
        provider.politeness.user_agent.clone(),
        solver,
        session_store,
    );
    cfg.rps = provider.politeness.rps;
    cfg.concurrency = provider.politeness.concurrency;
    cfg.robots = robots;
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

// ---------------------------------------------------------------------------
// External sync — admin visibility + operator actions (design: admin Sync console tab)
// ---------------------------------------------------------------------------

/// `GET /v1/admin/sync/accounts` — every linked external account across all users.
pub async fn list_sync_accounts(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<Vec<tankovault_db::repo::sync::AdminAccountRow>>> {
    user.require(UserRole::Operator)?;
    Ok(Json(
        tankovault_db::repo::sync::admin_list_accounts(&state.pool, 200).await?,
    ))
}

/// `GET /v1/admin/sync/mappings` — every series↔external mapping across all providers.
pub async fn list_sync_mappings(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<Vec<tankovault_db::repo::sync::AdminMappingRow>>> {
    user.require(UserRole::Operator)?;
    Ok(Json(
        tankovault_db::repo::sync::admin_list_mappings(&state.pool, 200).await?,
    ))
}

#[derive(Debug, Deserialize)]
pub struct SyncAccountTarget {
    pub user_id: UserId,
    pub provider: String,
}

/// `POST /v1/admin/sync/pull` — operator-forced pull for another user's linked account.
pub async fn admin_sync_pull(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<SyncAccountTarget>,
) -> ApiResult<Json<serde_json::Value>> {
    user.require(UserRole::Operator)?;
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

/// `POST /v1/admin/sync/push` — operator-forced push for another user's linked account.
pub async fn admin_sync_push(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<SyncAccountTarget>,
) -> ApiResult<Json<serde_json::Value>> {
    user.require(UserRole::Operator)?;
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

/// `POST /v1/admin/sync/unlink` — operator-forced unlink of another user's linked account.
pub async fn admin_sync_unlink(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<SyncAccountTarget>,
) -> ApiResult<Json<serde_json::Value>> {
    user.require(UserRole::Operator)?;
    let url = format!(
        "{}/v1/sync/{}/link",
        state.sync_url.trim_end_matches('/'),
        req.provider
    );
    let resp = state
        .http
        .delete(url)
        .json(&serde_json::json!({ "user_id": req.user_id }))
        .send()
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "sync service unreachable");
            ApiError::Internal
        })?;
    if !resp.status().is_success() {
        return Err(ApiError::Internal);
    }
    let value: serde_json::Value = resp.json().await.map_err(|_| ApiError::Internal)?;
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

#[derive(Debug, Deserialize)]
pub struct SyncMappingTarget {
    pub series_id: SeriesId,
    pub provider: String,
}

/// `POST /v1/admin/sync/mappings/clear` — remove a bad series↔external mapping; the next
/// pull/push (or targeted push) re-resolves it from scratch.
pub async fn clear_sync_mapping(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<SyncMappingTarget>,
) -> ApiResult<Json<serde_json::Value>> {
    user.require(UserRole::Operator)?;
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

#[derive(Debug, Deserialize)]
pub struct UpsertMapping {
    pub series_id: SeriesId,
    pub provider: String,
    pub external_id: String,
}

/// `POST /v1/admin/sync/mappings` — manually create or correct a series↔external mapping
/// (design: admin Sync console tab). Lets an operator fix a wrong external id or add a
/// missing one by hand from the per-series "manga info" editor and the assign queue.
pub async fn upsert_sync_mapping(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<UpsertMapping>,
) -> ApiResult<Json<serde_json::Value>> {
    user.require(UserRole::Operator)?;
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

/// `GET /v1/admin/sync/series/{id}` — every external mapping recorded for one series, so the
/// console can render a per-series "manga info" panel showing what it is (and is not) synced to.
pub async fn list_sync_mappings_for_series(
    State(state): State<AppState>,
    user: AuthUser,
    Path(series_id): Path<SeriesId>,
) -> ApiResult<Json<Vec<tankovault_db::repo::sync::AdminMappingRow>>> {
    user.require(UserRole::Operator)?;
    Ok(Json(
        tankovault_db::repo::sync::admin_list_mappings_for_series(&state.pool, series_id).await?,
    ))
}

#[derive(Debug, Deserialize)]
pub struct UnmappedQuery {
    /// External provider to check membership against (e.g. `anilist`).
    pub provider: String,
    /// Optional case-insensitive title filter.
    #[serde(default)]
    pub query: Option<String>,
}

/// `GET /v1/admin/sync/unmapped` — the assign queue: canonical series without a mapping for
/// the given provider, richest first, so operators can review and hand-assign the ones the
/// automatic matcher was not confident enough to link.
pub async fn list_unmapped_series(
    State(state): State<AppState>,
    user: AuthUser,
    axum::extract::Query(q): axum::extract::Query<UnmappedQuery>,
) -> ApiResult<Json<Vec<tankovault_db::repo::sync::UnmappedSeriesRow>>> {
    user.require(UserRole::Operator)?;
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

/// `GET /v1/admin/sync/unmatched` — the reverse assign queue: remote provider entries a pull
/// fetched but the auto-matcher could not confidently link to a local series, so an operator
/// can reconcile **every** loaded entry by hand (not just the confident matches).
pub async fn list_unmatched_remote(
    State(state): State<AppState>,
    user: AuthUser,
    axum::extract::Query(q): axum::extract::Query<UnmappedQuery>,
) -> ApiResult<Json<Vec<tankovault_db::repo::sync::RemoteEntryRow>>> {
    user.require(UserRole::Operator)?;
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

#[derive(Debug, Deserialize)]
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
#[derive(Debug, Serialize)]
pub struct SuggestedMatch {
    pub series_id: Uuid,
    pub title: String,
    pub content_type: String,
    pub release_year: Option<i32>,
    pub source_count: i64,
    pub score: f32,
}

/// `GET /v1/admin/sync/suggest` — rank local catalogue series as likely matches for a fetched
/// remote entry, so the operator gets automatic suggestions instead of blind-searching. Uses
/// the same trigram candidates as auto-matching but returns the *full* ranked list (with
/// scores) rather than only confident ones, so even weak-but-plausible matches are offered.
pub async fn list_suggestions(
    State(state): State<AppState>,
    user: AuthUser,
    axum::extract::Query(q): axum::extract::Query<SuggestQuery>,
) -> ApiResult<Json<Vec<SuggestedMatch>>> {
    user.require(UserRole::Operator)?;
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

#[derive(Debug, Deserialize)]
pub struct AssignRemoteEntry {
    pub user_id: UserId,
    pub provider: String,
    pub external_id: String,
    pub series_id: SeriesId,
}

/// `POST /v1/admin/sync/assign` — hand-assign a fetched remote entry to a local series. It
/// records the mapping, imports the entry onto the user's watchlist (status + progress from
/// the stored snapshot) so the result shows immediately, and clears it from the unmatched
/// queue — no fresh pull required.
pub async fn assign_remote_entry(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<AssignRemoteEntry>,
) -> ApiResult<Json<serde_json::Value>> {
    user.require(UserRole::Operator)?;
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

/// Best-effort audit record; a logging failure must not fail the action.
async fn audit(
    state: &AppState,
    user: &AuthUser,
    action: &str,
    target: &str,
    detail: &serde_json::Value,
) {
    if let Err(e) = tankovault_db::repo::audit::record(
        &state.pool,
        Some(user.user_id),
        action,
        Some(target),
        detail,
    )
    .await
    {
        tracing::warn!(error = %e, action, "failed to write audit record");
    }
}
