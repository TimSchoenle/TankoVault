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

pub async fn series_detail(id: &str) -> ApiResult<SeriesDetail> {
    get(&format!("/v1/series/{id}"), None).await
}

pub async fn series_chapters(id: &str, source: Option<&str>) -> ApiResult<Vec<ChapterDto>> {
    let mut path = format!("/v1/series/{id}/chapters");
    if let Some(s) = source {
        path.push_str(&format!("?source={s}"));
    }
    get(&path, None).await
}

/// All genres/tags (public). Reserved for the Search screen's tag grouping (§17.2.6).
#[allow(dead_code)]
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
// Admin / operator console
// ---------------------------------------------------------------------------

pub async fn providers(token: &str) -> ApiResult<Vec<Provider>> {
    get("/v1/admin/providers", Some(token)).await
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
