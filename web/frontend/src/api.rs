//! Typed API client (design §17.4). Thin wrapper over `gloo-net`'s fetch that:
//! - targets the API service under the same origin (`/v1/...`);
//! - attaches the in-memory access token as a `Bearer` header;
//! - decodes JSON into the [`crate::models`] DTOs;
//! - normalises transport/decoding failures into a human `String` so views can render a
//!   named error state instead of a spinner-forever (design §17.3).

use crate::models::*;
use gloo_net::http::{Request, RequestBuilder, Response};
use serde::de::DeserializeOwned;
use serde::Serialize;

/// API origin prefix. Empty = same origin as the served SPA (the API/CDN deployment,
/// design §19). Kept a const so it is trivial to point at a dev backend if needed.
const API_BASE: &str = "";

pub type ApiResult<T> = Result<T, String>;

fn url(path: &str) -> String {
    format!("{API_BASE}{path}")
}

fn auth_header(req: RequestBuilder, token: Option<&str>) -> RequestBuilder {
    match token {
        Some(t) if !t.is_empty() => req.header("Authorization", &format!("Bearer {t}")),
        _ => req,
    }
}

async fn decode<T: DeserializeOwned>(resp: Response) -> ApiResult<T> {
    if !resp.ok() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(friendly_error(status, &body));
    }
    resp.json::<T>()
        .await
        .map_err(|e| format!("Could not read the server response ({e})."))
}

fn friendly_error(status: u16, body: &str) -> String {
    // The API returns `{ "error": "..." }` (services/api/src/error.rs); surface it when
    // present, otherwise a status-appropriate message.
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
        if let Some(msg) = v.get("error").and_then(|e| e.as_str()) {
            return msg.to_owned();
        }
    }
    match status {
        401 => "You need to sign in to do that.".to_owned(),
        403 => "You don't have permission to do that.".to_owned(),
        404 => "Not found.".to_owned(),
        409 => "That conflicts with the current state.".to_owned(),
        s if s >= 500 => "The server had a problem. Please retry.".to_owned(),
        _ => format!("Request failed ({status})."),
    }
}

async fn get<T: DeserializeOwned>(path: &str, token: Option<&str>) -> ApiResult<T> {
    let resp = auth_header(Request::get(&url(path)), token)
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?;
    decode(resp).await
}

async fn send_json<B: Serialize, T: DeserializeOwned>(
    method: Method,
    path: &str,
    body: &B,
    token: Option<&str>,
) -> ApiResult<T> {
    let base = match method {
        Method::Post => Request::post(&url(path)),
        Method::Put => Request::put(&url(path)),
        Method::Patch => Request::patch(&url(path)),
    };
    let request = auth_header(base, token)
        .json(body)
        .map_err(|e| format!("Could not encode the request ({e})."))?;
    let resp = request
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?;
    decode(resp).await
}

async fn delete_empty(path: &str, token: Option<&str>) -> ApiResult<()> {
    let resp = auth_header(Request::delete(&url(path)), token)
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?;
    if resp.ok() {
        Ok(())
    } else {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        Err(friendly_error(status, &body))
    }
}

enum Method {
    Post,
    Put,
    Patch,
}

// ---------------------------------------------------------------------------
// Auth
// ---------------------------------------------------------------------------

pub async fn login(req: &LoginRequest) -> ApiResult<TokenResponse> {
    send_json(Method::Post, "/v1/auth/login", req, None).await
}

pub async fn register(req: &RegisterRequest) -> ApiResult<TokenResponse> {
    send_json(Method::Post, "/v1/auth/register", req, None).await
}

/// Silent refresh using the httpOnly refresh cookie (design §17.4). No body.
pub async fn refresh() -> ApiResult<TokenResponse> {
    let resp = Request::post(&url("/v1/auth/refresh"))
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?;
    decode(resp).await
}

pub async fn logout() -> ApiResult<()> {
    let resp = Request::post(&url("/v1/auth/logout"))
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?;
    if resp.ok() {
        Ok(())
    } else {
        Err(friendly_error(resp.status(), ""))
    }
}

// ---------------------------------------------------------------------------
// Public catalogue
// ---------------------------------------------------------------------------

pub async fn list_series(query: Option<&str>, limit: i64) -> ApiResult<Vec<SeriesSummary>> {
    let mut path = format!("/v1/series?limit={limit}");
    if let Some(q) = query.filter(|q| !q.trim().is_empty()) {
        path.push_str(&format!("&query={}", urlencode(q)));
    }
    get(&path, None).await
}

/// Filter/sort/paginate the browse list server-side (§9.1). The JSON body is a plain
/// `SeriesSummary[]`; the match total and next page ride on the `X-Total-Count` /
/// `X-Next-Cursor` response headers, which we surface as [`SeriesPage`].
pub async fn list_series_filtered(filter: &SeriesFilter) -> ApiResult<SeriesPage> {
    let mut qs: Vec<String> = Vec::new();
    qs.push(format!("limit={}", filter.limit.clamp(1, 100)));
    qs.push(format!("page={}", filter.page.max(0)));
    if let Some(q) = filter.query.as_deref().filter(|q| !q.trim().is_empty()) {
        qs.push(format!("query={}", urlencode(q)));
    }
    if let Some(ct) = filter.content_type {
        qs.push(format!("content_type={}", ct.token()));
    }
    if let Some(st) = filter.status {
        qs.push(format!("status={}", st.token()));
    }
    if let Some(p) = filter.provider.as_deref().filter(|p| !p.is_empty()) {
        qs.push(format!("provider={}", urlencode(p)));
    }
    for t in &filter.tags {
        qs.push(format!("tag={}", urlencode(t)));
    }
    for t in &filter.exclude_tags {
        qs.push(format!("exclude_tag={}", urlencode(t)));
    }
    if let Some(y) = filter.year_min {
        qs.push(format!("year_min={y}"));
    }
    if let Some(y) = filter.year_max {
        qs.push(format!("year_max={y}"));
    }
    if let Some(c) = filter.min_chapters.filter(|c| *c > 0) {
        qs.push(format!("min_chapters={c}"));
    }
    if let Some(s) = filter.sort.as_deref().filter(|s| !s.is_empty()) {
        qs.push(format!("sort={s}"));
    }
    let path = format!("/v1/series?{}", qs.join("&"));

    let resp = Request::get(&url(&path))
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?;
    if !resp.ok() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(friendly_error(status, &body));
    }
    let headers = resp.headers();
    let total = headers
        .get("x-total-count")
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);
    let next_cursor = headers
        .get("x-next-cursor")
        .and_then(|v| v.parse::<i64>().ok());
    let items = resp
        .json::<Vec<SeriesSummary>>()
        .await
        .map_err(|e| format!("Could not read the server response ({e})."))?;
    let total = if total == 0 {
        i64::try_from(items.len()).unwrap_or(0)
    } else {
        total
    };
    Ok(SeriesPage {
        items,
        total,
        next_cursor,
    })
}

/// Public provider list + per-provider series counts for the Discover filter (§9.3).
pub async fn public_providers() -> ApiResult<Vec<PublicProvider>> {
    get("/v1/providers", None).await
}

pub async fn series_detail(id: &str) -> ApiResult<SeriesDetail> {
    get(&format!("/v1/series/{id}"), None).await
}

/// Chapter list for a series' source. When `token` is supplied the per-chapter `read`
/// flag is populated from the user's progress (§9.2); anonymous callers omit it.
pub async fn series_chapters(
    id: &str,
    source: Option<&str>,
    token: Option<&str>,
) -> ApiResult<Vec<ChapterDto>> {
    let mut path = format!("/v1/series/{id}/chapters");
    if let Some(s) = source {
        path.push_str(&format!("?source={s}"));
    }
    get(&path, token).await
}

/// All genres/tags (public). Used by the Discover filter and Series tag chips.
pub async fn tags() -> ApiResult<Vec<Tag>> {
    get("/v1/tags", None).await
}

// ---------------------------------------------------------------------------
// Me (authenticated)
// ---------------------------------------------------------------------------

pub async fn watchlist(token: &str) -> ApiResult<Vec<WatchlistItem>> {
    get("/v1/me/watchlist", Some(token)).await
}

pub async fn set_watchlist(
    token: &str,
    series_id: &str,
    body: &WatchlistUpsert,
) -> ApiResult<serde_json::Value> {
    send_json(
        Method::Put,
        &format!("/v1/me/watchlist/{series_id}"),
        body,
        Some(token),
    )
    .await
}

pub async fn remove_watchlist(token: &str, series_id: &str) -> ApiResult<()> {
    delete_empty(&format!("/v1/me/watchlist/{series_id}"), Some(token)).await
}

pub async fn set_progress(
    token: &str,
    series_id: &str,
    last_read_number: f64,
) -> ApiResult<serde_json::Value> {
    send_json(
        Method::Put,
        &format!("/v1/me/progress/{series_id}"),
        &ProgressUpdate { last_read_number },
        Some(token),
    )
    .await
}

pub async fn feed(token: &str) -> ApiResult<Vec<FeedEntry>> {
    get("/v1/me/feed", Some(token)).await
}

/// Continue-reading cards for Home / the Series CTA (§9.3).
pub async fn continue_reading(token: &str) -> ApiResult<Vec<ContinueItem>> {
    get("/v1/me/continue", Some(token)).await
}

/// "Because you read" recommendations (§9.3).
pub async fn recommendations(token: &str) -> ApiResult<Vec<SeriesSummary>> {
    get("/v1/me/recommendations", Some(token)).await
}

/// Lifetime tracking stats for the Home / Profile headline (§9.3).
pub async fn me_stats(token: &str) -> ApiResult<MeStats> {
    get("/v1/me/stats", Some(token)).await
}

/// Update the caller's username and/or email (§9.4). A duplicate surfaces as `409`.
pub async fn patch_profile(
    token: &str,
    username: Option<&str>,
    email: Option<&str>,
) -> ApiResult<ProfileDto> {
    let body = serde_json::json!({ "username": username, "email": email });
    send_json(Method::Patch, "/v1/me/profile", &body, Some(token)).await
}

/// The caller's active login sessions (§9.4).
pub async fn sessions(token: &str) -> ApiResult<Vec<SessionDto>> {
    get("/v1/me/sessions", Some(token)).await
}

/// Revoke one of the caller's own sessions (§9.4).
pub async fn delete_session(token: &str, id: &str) -> ApiResult<()> {
    delete_empty(&format!("/v1/me/sessions/{id}"), Some(token)).await
}

/// Read the caller's notification preferences JSON (§9.4).
pub async fn notification_prefs(token: &str) -> ApiResult<serde_json::Value> {
    get("/v1/me/notification-prefs", Some(token)).await
}

/// Replace the caller's notification preferences JSON (§9.4).
pub async fn set_notification_prefs(
    token: &str,
    prefs: &serde_json::Value,
) -> ApiResult<serde_json::Value> {
    send_json(Method::Put, "/v1/me/notification-prefs", prefs, Some(token)).await
}

pub async fn notifications(token: &str) -> ApiResult<Vec<Notification>> {
    get("/v1/me/notifications", Some(token)).await
}

/// URL of the live-notification SSE stream. The access token rides in the query string
/// because the browser `EventSource` API cannot set an `Authorization` header (design §17.4).
pub fn stream_url(token: &str) -> String {
    url(&format!("/v1/me/stream?access_token={}", urlencode(token)))
}

pub async fn mark_read(token: &str, ids: &[String]) -> ApiResult<serde_json::Value> {
    send_json(
        Method::Post,
        "/v1/me/notifications/read",
        &serde_json::json!({ "ids": ids }),
        Some(token),
    )
    .await
}

// ---------------------------------------------------------------------------
// External sync (Sync & integrations panel, header pill, Watchlist, Series tracking card).
// Provider-parameterized (design: generalized multi-provider sync) — AniList is the only
// registered provider today, reached via `provider: "anilist"`.
// ---------------------------------------------------------------------------

/// The registered external sync providers.
pub async fn sync_providers(token: &str) -> ApiResult<Vec<ProviderInfo>> {
    get("/v1/me/sync/providers", Some(token)).await
}

/// `provider`'s OAuth consent URL to send the browser to.
pub async fn sync_authorize_url(token: &str, provider: &str) -> ApiResult<String> {
    let v: serde_json::Value =
        get(&format!("/v1/me/sync/{provider}/authorize"), Some(token)).await?;
    v.get("url")
        .and_then(|u| u.as_str())
        .map(str::to_owned)
        .ok_or_else(|| "The provider did not return an authorize URL.".to_owned())
}

/// Whether the caller has a linked `provider` account, plus username/last-sync.
pub async fn sync_status(token: &str, provider: &str) -> ApiResult<SyncStatus> {
    get(&format!("/v1/me/sync/{provider}/status"), Some(token)).await
}

/// Exchange an OAuth `code` (captured from `provider`'s redirect) for a linked account.
pub async fn sync_link(token: &str, provider: &str, code: &str) -> ApiResult<()> {
    let _: serde_json::Value = get(
        &format!("/v1/me/sync/{provider}/callback?code={}", urlencode(code)),
        Some(token),
    )
    .await?;
    Ok(())
}

/// Unlink the caller's `provider` account.
pub async fn sync_disconnect(token: &str, provider: &str) -> ApiResult<()> {
    delete_empty(&format!("/v1/me/sync/{provider}"), Some(token)).await
}

/// Import `provider`'s list into the local watchlist/progress under `policy`.
pub async fn sync_pull(
    token: &str,
    provider: &str,
    policy: ConflictPolicy,
) -> ApiResult<serde_json::Value> {
    send_json(
        Method::Post,
        &format!("/v1/me/sync/{provider}/pull"),
        &serde_json::json!({ "policy": policy.token() }),
        Some(token),
    )
    .await
}

/// Reflect the local watchlist/progress to `provider` under `policy`.
pub async fn sync_push(
    token: &str,
    provider: &str,
    policy: ConflictPolicy,
) -> ApiResult<serde_json::Value> {
    send_json(
        Method::Post,
        &format!("/v1/me/sync/{provider}/push"),
        &serde_json::json!({ "policy": policy.token() }),
        Some(token),
    )
    .await
}

// ---------------------------------------------------------------------------
// Admin / operator console — Sync tab (design: admin Sync console tab)
// ---------------------------------------------------------------------------

pub async fn admin_sync_accounts(token: &str) -> ApiResult<Vec<AdminSyncAccount>> {
    get("/v1/admin/sync/accounts", Some(token)).await
}

pub async fn admin_sync_mappings(token: &str) -> ApiResult<Vec<AdminSyncMapping>> {
    get("/v1/admin/sync/mappings", Some(token)).await
}

pub async fn admin_sync_pull(
    token: &str,
    user_id: &str,
    provider: &str,
) -> ApiResult<serde_json::Value> {
    send_json(
        Method::Post,
        "/v1/admin/sync/pull",
        &serde_json::json!({ "user_id": user_id, "provider": provider }),
        Some(token),
    )
    .await
}

pub async fn admin_sync_push(
    token: &str,
    user_id: &str,
    provider: &str,
) -> ApiResult<serde_json::Value> {
    send_json(
        Method::Post,
        "/v1/admin/sync/push",
        &serde_json::json!({ "user_id": user_id, "provider": provider }),
        Some(token),
    )
    .await
}

pub async fn admin_sync_unlink(
    token: &str,
    user_id: &str,
    provider: &str,
) -> ApiResult<serde_json::Value> {
    send_json(
        Method::Post,
        "/v1/admin/sync/unlink",
        &serde_json::json!({ "user_id": user_id, "provider": provider }),
        Some(token),
    )
    .await
}

pub async fn admin_clear_sync_mapping(
    token: &str,
    series_id: &str,
    provider: &str,
) -> ApiResult<serde_json::Value> {
    send_json(
        Method::Post,
        "/v1/admin/sync/mappings/clear",
        &serde_json::json!({ "series_id": series_id, "provider": provider }),
        Some(token),
    )
    .await
}

// ---------------------------------------------------------------------------
// Admin / operator console
// ---------------------------------------------------------------------------

pub async fn providers(token: &str) -> ApiResult<Vec<Provider>> {
    get("/v1/admin/providers", Some(token)).await
}

/// The operator Users directory: identity, role, and tracked-series count (§9.5).
pub async fn admin_users(token: &str) -> ApiResult<Vec<UserRow>> {
    get("/v1/admin/users", Some(token)).await
}

/// Operator "Re-solve" — queue a fast re-scan that re-attempts challenged sources (§9.5,
/// audited server-side as `provider.resolve`).
pub async fn resolve_provider(token: &str, id: &str) -> ApiResult<serde_json::Value> {
    send_json(
        Method::Post,
        &format!("/v1/admin/providers/{id}/resolve"),
        &serde_json::json!({}),
        Some(token),
    )
    .await
}

/// Full provider edit (PATCH). Changing `base_url` performs the domain migration; `config`
/// and `politeness` are JSON objects validated (and politeness-clamped) server-side.
pub async fn update_provider(
    token: &str,
    id: &str,
    name: &str,
    base_url: &str,
    config: &serde_json::Value,
    politeness: &serde_json::Value,
) -> ApiResult<Provider> {
    send_json(
        Method::Patch,
        &format!("/v1/admin/providers/{id}"),
        &serde_json::json!({
            "name": name,
            "base_url": base_url,
            "config": config,
            "politeness": politeness,
        }),
        Some(token),
    )
    .await
}

/// Create a provider (admin only). Pass `politeness = None` to accept the polite server
/// defaults rather than sending explicit values.
pub async fn create_provider(
    token: &str,
    slug: &str,
    name: &str,
    base_url: &str,
    adapter: &str,
    config: &serde_json::Value,
    politeness: Option<&serde_json::Value>,
) -> ApiResult<Provider> {
    let mut body = serde_json::json!({
        "slug": slug,
        "name": name,
        "base_url": base_url,
        "adapter": adapter,
        "config": config,
    });
    if let Some(p) = politeness {
        body["politeness"] = p.clone();
    }
    send_json(Method::Post, "/v1/admin/providers", &body, Some(token)).await
}

/// Delete a provider (admin only). Its source links cascade-delete server-side.
pub async fn delete_provider(token: &str, id: &str) -> ApiResult<()> {
    delete_empty(&format!("/v1/admin/providers/{id}"), Some(token)).await
}

/// Override a provider's health state (`disabled` to pause crawling, `active` to re-enable).
pub async fn set_provider_state(token: &str, id: &str, state: &str) -> ApiResult<Provider> {
    send_json(
        Method::Post,
        &format!("/v1/admin/providers/{id}/state"),
        &serde_json::json!({ "state": state }),
        Some(token),
    )
    .await
}

/// Dry-run a provider's adapter against the live site (design §11). `path`, if set, also
/// fetches one series' metadata + chapters. Returns the raw sample JSON for display.
pub async fn test_adapter(
    token: &str,
    id: &str,
    path: Option<&str>,
) -> ApiResult<serde_json::Value> {
    let body = match path {
        Some(p) if !p.is_empty() => serde_json::json!({ "path": p }),
        _ => serde_json::json!({}),
    };
    send_json(
        Method::Post,
        &format!("/v1/admin/providers/{id}/test"),
        &body,
        Some(token),
    )
    .await
}

pub async fn trigger_scan(
    token: &str,
    provider_id: Option<&str>,
    mode: ScanMode,
) -> ApiResult<serde_json::Value> {
    let mode = match mode {
        ScanMode::Full => "full",
        ScanMode::Fast => "fast",
    };
    let body = match provider_id {
        Some(p) => serde_json::json!({ "provider_id": p, "mode": mode }),
        None => serde_json::json!({ "mode": mode }),
    };
    send_json(Method::Post, "/v1/admin/scans", &body, Some(token)).await
}

/// System-wide rollup for the console header.
pub async fn system_stats(token: &str) -> ApiResult<SystemStats> {
    get("/v1/admin/stats", Some(token)).await
}

/// Per-provider crawl statistics (richest first).
pub async fn provider_stats(token: &str) -> ApiResult<Vec<ProviderStat>> {
    get("/v1/admin/providers/stats", Some(token)).await
}

/// The most recent scan runs (the console's scan-queue overview).
pub async fn recent_runs(token: &str) -> ApiResult<Vec<ScanRun>> {
    get("/v1/admin/scans", Some(token)).await
}

/// The most recently failed scan tasks with their errors.
pub async fn scan_failures(token: &str) -> ApiResult<Vec<FailedTask>> {
    get("/v1/admin/scan-failures", Some(token)).await
}

/// The most recent privileged actions from the audit trail.
pub async fn audit_log(token: &str) -> ApiResult<Vec<AuditEntry>> {
    get("/v1/admin/audit", Some(token)).await
}

pub async fn merge_candidates(token: &str) -> ApiResult<Vec<MergeCandidate>> {
    get("/v1/admin/merge-candidates", Some(token)).await
}

pub async fn dismiss_candidate(token: &str, id: &str) -> ApiResult<serde_json::Value> {
    send_json(
        Method::Post,
        "/v1/admin/merge-candidates/dismiss",
        &serde_json::json!({ "id": id }),
        Some(token),
    )
    .await
}

pub async fn merge_series(token: &str, keep: &str, merge: &str) -> ApiResult<serde_json::Value> {
    send_json(
        Method::Post,
        "/v1/admin/series/merge",
        &serde_json::json!({ "keep": keep, "merge": merge }),
        Some(token),
    )
    .await
}

/// Minimal percent-encoding for a query value (space + the handful of reserved chars we
/// actually pass through). Avoids pulling a URL crate into the WASM bundle.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
