//! Typed API client (design §17.4). Wrapper over the generated `tankovault-api-client`.
//!
//! targets the API service under the same origin (`/v1/...`) via `HttpClient`.

use crate::models::*;
use crate::state::Session;
use dioxus::prelude::*;
use tankovault_api_client::generated::client::{ApiOpError, HttpClient};
use tankovault_api_client::generated::types::*;

pub type ApiResult<T> = Result<T, String>;

#[derive(Clone)]
pub struct ApiClient(pub HttpClient);

pub fn use_api() -> HttpClient {
    let session = use_context::<Session>();
    let client = use_context::<ApiClient>();
    
    let mut http_client = client.0.clone();
    if let Some(token) = session.token_value() {
        http_client = http_client.with_api_key(token);
    }
    http_client
}

pub fn provide_api() {
    let client = HttpClient::new()
        .with_base_url(""); // Same origin
    use_context_provider(|| ApiClient(client));
}

pub fn friendly_error<E: std::fmt::Debug>(err: ApiOpError<E>) -> String {
    match err {
        ApiOpError::Api(api_err) => {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&api_err.body) {
                if let Some(msg) = v.get("error").and_then(|e| e.as_str()) {
                    return msg.to_owned();
                }
            }
            match api_err.status {
                401 => "You need to sign in to do that.".to_owned(),
                403 => "You don't have permission to do that.".to_owned(),
                404 => "Not found.".to_owned(),
                409 => "That conflicts with the current state.".to_owned(),
                s if s >= 500 => "The server had a problem. Please retry.".to_owned(),
                status => format!("Request failed ({status})."),
            }
        }
        ApiOpError::Transport(trans_err) => format!("Network error: {trans_err}"),
    }
}

// ---------------------------------------------------------------------------
// Legacy shim
// ---------------------------------------------------------------------------

pub async fn login(req: &LoginRequest) -> ApiResult<TokenResponse> {
    let client = HttpClient::new().with_base_url("");
    client.login(req.clone())
        .await
        .map_err(friendly_error)
}

pub async fn register(req: &RegisterRequest) -> ApiResult<TokenResponse> {
    let client = HttpClient::new().with_base_url("");
    client.register(req.clone())
        .await
        .map_err(friendly_error)
}

pub async fn refresh() -> ApiResult<TokenResponse> {
    let client = HttpClient::new().with_base_url("");
    client.refresh()
        .await
        .map_err(friendly_error)
}

pub async fn logout() -> ApiResult<()> {
    let client = HttpClient::new().with_base_url("");
    client.logout()
        .await
        .map(|_| ())
        .map_err(friendly_error)
}

pub async fn list_series(query: Option<&str>, limit: i64) -> ApiResult<Vec<SeriesSummary>> {
    let client = HttpClient::new().with_base_url("");
    let mut builder = client.list_builder().limit(limit);
    if let Some(q) = query {
        builder = builder.query(q);
    }
    builder.send()
        .await
        .map_err(friendly_error)
}

pub async fn list_series_filtered(filter: &SeriesFilter) -> ApiResult<Vec<SeriesSummary>> {
    let client = HttpClient::new().with_base_url("");
    let mut builder = client.list_builder()
        .limit(filter.limit.unwrap_or(40));
    
    if let Some(q) = &filter.query {
        builder = builder.query(q.clone());
    }
    if let Some(ct) = &filter.content_type {
        builder = builder.content_type(ct.clone());
    }
    if let Some(st) = &filter.status {
        builder = builder.status(st.clone());
    }
    if let Some(p) = &filter.provider {
        builder = builder.provider(p.clone());
    }
    if let Some(y) = filter.year_min {
        builder = builder.year_min(y as i32);
    }
    if let Some(y) = filter.year_max {
        builder = builder.year_max(y as i32);
    }
    if let Some(m) = filter.min_chapters {
        builder = builder.min_chapters(m as i32);
    }
    if let Some(s) = &filter.sort {
        builder = builder.sort(s.clone());
    }
    if let Some(p) = filter.page {
        builder = builder.page(p);
    }

    builder.send()
        .await
        .map_err(friendly_error)
}

pub async fn series_detail(id: SeriesId) -> ApiResult<SeriesDetail> {
    let client = HttpClient::new().with_base_url("");
    client.detail(id.to_string())
        .await
        .map_err(friendly_error)
}

pub async fn series_chapters(id: SeriesId, source: Option<&str>, token: Option<&str>) -> ApiResult<Vec<ChapterDto>> {
    let mut client = HttpClient::new().with_base_url("");
    if let Some(t) = token {
        client = client.with_api_key(t);
    }
    let mut builder = client.chapters_builder(id.to_string());
    if let Some(s) = source {
        builder = builder.source(s);
    }
    builder.send()
        .await
        .map_err(friendly_error)
}

pub async fn tags() -> ApiResult<Vec<Tag>> {
    let client = HttpClient::new().with_base_url("");
    client.tags()
        .await
        .map_err(friendly_error)
}

pub async fn public_providers() -> ApiResult<Vec<PublicProvider>> {
    let client = HttpClient::new().with_base_url("");
    client.providers()
        .await
        .map_err(friendly_error)
}

pub async fn watchlist(token: &str) -> ApiResult<Vec<WatchlistItem>> {
    let client = HttpClient::new().with_base_url("").with_api_key(token);
    client.watchlist()
        .await
        .map_err(friendly_error)
}

pub async fn set_watchlist(token: &str, series_id: SeriesId, upsert: &WatchlistUpsert) -> ApiResult<WatchlistItem> {
    let client = HttpClient::new().with_base_url("").with_api_key(token);
    client.put_watchlist_builder(series_id.to_string()).request(upsert.clone())
        .send()
        .await
        .map(|resp| {
             // Correctly extract WatchlistItem from response if needed
             // Map from PutWatchlistResponse to WatchlistItem
             match resp {
                 // Assuming PutWatchlistResponse::Ok(WatchlistItem) or similar
                 _ => serde_json::from_value(serde_json::to_value(resp).unwrap()).unwrap()
             }
        })
        .map_err(friendly_error)
}

pub async fn remove_watchlist(token: &str, series_id: SeriesId) -> ApiResult<()> {
    let client = HttpClient::new().with_base_url("").with_api_key(token);
    client.delete_watchlist(series_id.to_string())
        .await
        .map(|_| ())
        .map_err(friendly_error)
}

pub async fn set_progress(token: &str, series_id: SeriesId, update: &ProgressUpdate) -> ApiResult<ProgressDto> {
    let client = HttpClient::new().with_base_url("").with_api_key(token);
    client.put_progress_builder(series_id.to_string()).request(update.clone())
        .send()
        .await
        .map(|resp| serde_json::from_value(serde_json::to_value(resp).unwrap()).unwrap())
        .map_err(friendly_error)
}

pub async fn mark_chapter(token: &str, series_id: SeriesId, chapter_id: ChapterId, read: bool) -> ApiResult<ProgressDto> {
    let client = HttpClient::new().with_base_url("").with_api_key(token);
    client.put_chapter_progress_builder(series_id.to_string(), chapter_id.to_string()).request(ChapterRead { read })
        .send()
        .await
        .map(|resp| serde_json::from_value(serde_json::to_value(resp).unwrap()).unwrap())
        .map_err(friendly_error)
}

pub async fn mark_read_to(token: &str, series_id: SeriesId, chapter_id: ChapterId) -> ApiResult<ProgressDto> {
    let client = HttpClient::new().with_base_url("").with_api_key(token);
    client.mark_read_to_builder(series_id.to_string()).request(MarkReadTo { chapter_id })
        .send()
        .await
        .map(|resp| serde_json::from_value(serde_json::to_value(resp).unwrap()).unwrap())
        .map_err(friendly_error)
}

pub async fn feed(token: &str) -> ApiResult<Vec<FeedEntry>> {
    let client = HttpClient::new().with_base_url("").with_api_key(token);
    client.feed()
        .await
        .map_err(friendly_error)
}

pub async fn me_stats(token: &str) -> ApiResult<MeStats> {
    let client = HttpClient::new().with_base_url("").with_api_key(token);
    client.stats()
        .await
        .map_err(friendly_error)
}

pub async fn continue_reading(token: &str) -> ApiResult<Vec<ContinueItem>> {
    let client = HttpClient::new().with_base_url("").with_api_key(token);
    client.continue_reading()
        .await
        .map_err(friendly_error)
}

pub async fn recommendations(token: &str) -> ApiResult<Vec<SeriesSummary>> {
    let client = HttpClient::new().with_base_url("").with_api_key(token);
    client.recommendations()
        .await
        .map_err(friendly_error)
}

pub async fn sync_status(token: &str, provider: &str) -> ApiResult<SyncStatus> {
    let client = HttpClient::new().with_base_url("").with_api_key(token);
    client.sync_status(provider)
        .await
        .map(|resp| {
             serde_json::from_value(serde_json::to_value(resp).unwrap()).unwrap()
        })
        .map_err(friendly_error)
}

pub fn stream_url(token: &str) -> String {
    format!("/v1/me/stream?token={token}")
}

pub async fn patch_profile(token: &str, update: &ProfileUpdate) -> ApiResult<ProfileDto> {
    let client = HttpClient::new().with_base_url("").with_api_key(token);
    client.patch_profile_builder().request(update.clone())
        .send()
        .await
        .map(|resp| serde_json::from_value(serde_json::to_value(resp).unwrap()).unwrap())
        .map_err(friendly_error)
}

pub async fn sessions(token: &str) -> ApiResult<Vec<SessionDto>> {
    let client = HttpClient::new().with_base_url("").with_api_key(token);
    client.sessions()
        .await
        .map_err(friendly_error)
}

pub async fn delete_session(token: &str, session_id: &str) -> ApiResult<()> {
    let client = HttpClient::new().with_base_url("").with_api_key(token);
    client.delete_session(session_id)
        .await
        .map(|_| ())
        .map_err(friendly_error)
}

pub async fn notifications(token: &str) -> ApiResult<Vec<serde_json::Value>> {
    let client = HttpClient::new().with_base_url("").with_api_key(token);
    client.notifications()
        .await
        .map_err(friendly_error)
}

pub async fn mark_read(token: &str, id: Option<NotificationId>) -> ApiResult<()> {
    let client = HttpClient::new().with_base_url("").with_api_key(token);
    let mut builder = client.mark_read_builder();
    if let Some(nid) = id {
        builder = builder.id(nid.to_string());
    }
    builder.send()
        .await
        .map(|_| ())
        .map_err(friendly_error)
}

pub async fn notification_prefs(token: &str) -> ApiResult<serde_json::Value> {
    let client = HttpClient::new().with_base_url("").with_api_key(token);
    client.notification_prefs()
        .await
        .map_err(friendly_error)
}

pub async fn set_notification_prefs(token: &str, prefs: &serde_json::Value) -> ApiResult<serde_json::Value> {
    let client = HttpClient::new().with_base_url("").with_api_key(token);
    client.put_notification_prefs_builder().request(prefs.clone())
        .send()
        .await
        .map_err(friendly_error)
}

pub async fn sync_providers(token: &str) -> ApiResult<Vec<ProviderInfo>> {
    let client = HttpClient::new().with_base_url("").with_api_key(token);
    client.sync_providers()
        .await
        .map(|resp| {
             serde_json::from_value(serde_json::to_value(resp).unwrap()).unwrap()
        })
        .map_err(friendly_error)
}

pub async fn sync_authorize_url(token: &str, provider: &str) -> ApiResult<String> {
    let client = HttpClient::new().with_base_url("").with_api_key(token);
    client.sync_authorize_url(provider)
        .await
        .map(|resp| {
             serde_json::from_value(serde_json::to_value(resp).unwrap()).unwrap()
        })
        .map_err(friendly_error)
}

pub async fn sync_disconnect(token: &str, provider: &str) -> ApiResult<()> {
    let client = HttpClient::new().with_base_url("").with_api_key(token);
    client.sync_disconnect(provider)
        .await
        .map(|_| ())
        .map_err(friendly_error)
}

pub async fn sync_pull(token: &str, provider: &str, _policy: ConflictPolicy) -> ApiResult<()> {
    let client = HttpClient::new().with_base_url("").with_api_key(token);
    client.sync_pull(provider)
        .await
        .map(|_| ())
        .map_err(friendly_error)
}

pub async fn sync_push(token: &str, provider: &str, _policy: ConflictPolicy) -> ApiResult<()> {
    let client = HttpClient::new().with_base_url("").with_api_key(token);
    client.sync_push(provider)
        .await
        .map(|_| ())
        .map_err(friendly_error)
}

pub async fn sync_settings(token: &str, provider: &str) -> ApiResult<SyncSettings> {
    let client = HttpClient::new().with_base_url("").with_api_key(token);
    client.sync_settings(provider)
        .await
        .map(|resp| {
             serde_json::from_value(serde_json::to_value(resp).unwrap()).unwrap()
        })
        .map_err(friendly_error)
}

pub async fn patch_sync_settings(token: &str, provider: &str, patch: &SyncSettingsPatch) -> ApiResult<SyncSettings> {
    let client = HttpClient::new().with_base_url("").with_api_key(token);
    client.sync_settings_patch_builder(provider).request(patch.clone())
        .send()
        .await
        .map(|resp| {
             serde_json::from_value(serde_json::to_value(resp).unwrap()).unwrap()
        })
        .map_err(friendly_error)
}

pub async fn sync_conflicts(token: &str, provider: &str) -> ApiResult<Vec<SyncConflict>> {
    let client = HttpClient::new().with_base_url("").with_api_key(token);
    client.sync_conflicts(provider)
        .await
        .map(|resp| {
             serde_json::from_value(serde_json::to_value(resp).unwrap()).unwrap()
        })
        .map_err(friendly_error)
}

pub async fn sync_resolve_conflict(token: &str, provider: &str, resolve: &ResolveConflict) -> ApiResult<()> {
    let client = HttpClient::new().with_base_url("").with_api_key(token);
    client.sync_resolve_conflict_builder(provider).request(resolve.clone())
        .send()
        .await
        .map(|_| ())
        .map_err(friendly_error)
}

pub async fn sync_history(token: &str, provider: &str) -> ApiResult<Vec<SyncHistory>> {
    let client = HttpClient::new().with_base_url("").with_api_key(token);
    client.sync_history_builder()
        .provider(provider)
        .send()
        .await
        .map(|resp| {
             serde_json::from_value(serde_json::to_value(resp).unwrap()).unwrap()
        })
        .map_err(friendly_error)
}

// ---------------------------------------------------------------------------
// Admin
// ---------------------------------------------------------------------------

pub async fn list_providers(token: &str) -> ApiResult<Vec<PublicProvider>> {
    let client = HttpClient::new().with_base_url("").with_api_key(token);
    client.list_providers()
        .await
        .map_err(friendly_error)
}

pub async fn create_provider(token: &str, req: &CreateProvider) -> ApiResult<PublicProvider> {
    let client = HttpClient::new().with_base_url("").with_api_key(token);
    client.create_provider_builder().request(req.clone())
        .send()
        .await
        .map_err(friendly_error)
}

pub async fn update_provider(token: &str, id: ProviderId, req: &UpdateProvider) -> ApiResult<PublicProvider> {
    let client = HttpClient::new().with_base_url("").with_api_key(token);
    client.update_provider_builder(id.to_string()).request(req.clone())
        .send()
        .await
        .map_err(friendly_error)
}

pub async fn delete_provider(token: &str, id: ProviderId) -> ApiResult<()> {
    let client = HttpClient::new().with_base_url("").with_api_key(token);
    client.delete_provider(id.to_string())
        .await
        .map(|_| ())
        .map_err(friendly_error)
}

pub async fn provider_stats(token: &str, id: ProviderId) -> ApiResult<ProviderStat> {
    let client = HttpClient::new().with_base_url("").with_api_key(token);
    client.provider_stats(id.to_string())
        .await
        .map_err(friendly_error)
}

pub async fn system_stats(token: &str) -> ApiResult<SystemStats> {
    let client = HttpClient::new().with_base_url("").with_api_key(token);
    client.system_stats()
        .await
        .map_err(friendly_error)
}

pub async fn list_users(token: &str) -> ApiResult<Vec<UserRow>> {
    let client = HttpClient::new().with_base_url("").with_api_key(token);
    client.list_users()
        .await
        .map(|resp| {
             serde_json::from_value(serde_json::to_value(resp).unwrap()).unwrap()
        })
        .map_err(friendly_error)
}

pub async fn list_scans(token: &str) -> ApiResult<Vec<ScanRun>> {
    let client = HttpClient::new().with_base_url("").with_api_key(token);
    client.list_scans()
        .await
        .map_err(friendly_error)
}

pub async fn trigger_scan(token: &str, req: &TriggerScan) -> ApiResult<ScanRun> {
    let client = HttpClient::new().with_base_url("").with_api_key(token);
    client.trigger_scan_builder(req.mode.clone())
        .provider_id(req.provider_id.map(|id| id.to_string()).unwrap_or_default())
        .send()
        .await
        .map_err(friendly_error)
}

pub async fn list_merge_candidates(token: &str) -> ApiResult<Vec<MergeCandidate>> {
    let client = HttpClient::new().with_base_url("").with_api_key(token);
    client.list_merge_candidates()
        .await
        .map(|resp| {
             serde_json::from_value(serde_json::to_value(resp).unwrap()).unwrap()
        })
        .map_err(friendly_error)
}

pub async fn merge_series(token: &str, req: &MergeRequest) -> ApiResult<()> {
    let client = HttpClient::new().with_base_url("").with_api_key(token);
    client.merge_series(req.clone())
        .await
        .map(|_| ())
        .map_err(friendly_error)
}

pub async fn dismiss_merge_candidate(token: &str, req: &DismissRequest) -> ApiResult<()> {
    let client = HttpClient::new().with_base_url("").with_api_key(token);
    client.dismiss_merge_candidate(req.clone())
        .await
        .map(|_| ())
        .map_err(friendly_error)
}

pub async fn list_sync_accounts(token: &str) -> ApiResult<Vec<AdminSyncAccount>> {
    let client = HttpClient::new().with_base_url("").with_api_key(token);
    client.list_sync_accounts()
        .await
        .map(|resp| {
             serde_json::from_value(serde_json::to_value(resp).unwrap()).unwrap()
        })
        .map_err(friendly_error)
}

pub async fn list_sync_mappings(token: &str) -> ApiResult<Vec<AdminSyncMapping>> {
    let client = HttpClient::new().with_base_url("").with_api_key(token);
    client.list_sync_mappings()
        .await
        .map(|resp| {
             serde_json::from_value(serde_json::to_value(resp).unwrap()).unwrap()
        })
        .map_err(friendly_error)
}

pub async fn list_sync_mappings_for_series(token: &str, series_id: SeriesId) -> ApiResult<Vec<AdminSyncMapping>> {
    let client = HttpClient::new().with_base_url("").with_api_key(token);
    client.list_sync_mappings_for_series(series_id.to_string())
        .await
        .map(|resp| {
             serde_json::from_value(serde_json::to_value(resp).unwrap()).unwrap()
        })
        .map_err(friendly_error)
}

pub async fn upsert_sync_mapping(token: &str, req: &UpsertMapping) -> ApiResult<()> {
    let client = HttpClient::new().with_base_url("").with_api_key(token);
    client.upsert_sync_mapping(req.clone())
        .await
        .map(|_| ())
        .map_err(friendly_error)
}

pub async fn clear_sync_mapping(token: &str, provider: &str, series_id: SeriesId) -> ApiResult<()> {
    let client = HttpClient::new().with_base_url("").with_api_key(token);
    client.clear_sync_mapping_builder(provider, series_id.to_string())
        .send()
        .await
        .map(|_| ())
        .map_err(friendly_error)
}

pub async fn list_unmapped_series(token: &str) -> ApiResult<Vec<UnmappedSeries>> {
    let client = HttpClient::new().with_base_url("").with_api_key(token);
    client.list_unmapped_series()
        .await
        .map(|resp| {
             serde_json::from_value(serde_json::to_value(resp).unwrap()).unwrap()
        })
        .map_err(friendly_error)
}

pub async fn list_unmatched_remote(token: &str, provider: &str) -> ApiResult<Vec<UnmatchedRemoteEntry>> {
    let client = HttpClient::new().with_base_url("").with_api_key(token);
    client.list_unmatched_remote(provider)
        .await
        .map(|resp| {
             serde_json::from_value(serde_json::to_value(resp).unwrap()).unwrap()
        })
        .map_err(friendly_error)
}

pub async fn list_suggestions(token: &str, provider: &str) -> ApiResult<Vec<SuggestedMatch>> {
    let client = HttpClient::new().with_base_url("").with_api_key(token);
    client.list_suggestions(provider)
        .await
        .map(|resp| {
             serde_json::from_value(serde_json::to_value(resp).unwrap()).unwrap()
        })
        .map_err(friendly_error)
}

pub async fn assign_remote_entry(token: &str, req: &AssignRemoteEntry) -> ApiResult<()> {
    let client = HttpClient::new().with_base_url("").with_api_key(token);
    client.assign_remote_entry(req.clone())
        .await
        .map(|_| ())
        .map_err(friendly_error)
}

pub async fn admin_sync_pull(token: &str, provider: &str, user_id: UserId) -> ApiResult<()> {
    let client = HttpClient::new().with_base_url("").with_api_key(token);
    client.admin_sync_pull_builder(provider, user_id.to_string())
        .send()
        .await
        .map(|_| ())
        .map_err(friendly_error)
}

pub async fn admin_sync_push(token: &str, provider: &str, user_id: UserId) -> ApiResult<()> {
    let client = HttpClient::new().with_base_url("").with_api_key(token);
    client.admin_sync_push_builder(provider, user_id.to_string())
        .send()
        .await
        .map(|_| ())
        .map_err(friendly_error)
}

pub async fn admin_sync_unlink(token: &str, provider: &str, user_id: UserId) -> ApiResult<()> {
    let client = HttpClient::new().with_base_url("").with_api_key(token);
    client.admin_sync_unlink_builder(provider, user_id.to_string())
        .send()
        .await
        .map(|_| ())
        .map_err(friendly_error)
}

pub async fn audit_log(token: &str) -> ApiResult<Vec<AuditEntry>> {
    let client = HttpClient::new().with_base_url("").with_api_key(token);
    client.audit_log()
        .await
        .map(|resp| {
             serde_json::from_value(serde_json::to_value(resp).unwrap()).unwrap()
        })
        .map_err(friendly_error)
}

pub async fn scan_failures(token: &str) -> ApiResult<Vec<FailedTask>> {
    let client = HttpClient::new().with_base_url("").with_api_key(token);
    client.scan_failures()
        .await
        .map(|resp| {
             serde_json::from_value(serde_json::to_value(resp).unwrap()).unwrap()
        })
        .map_err(friendly_error)
}

pub async fn scan_stream() -> String {
    "/v1/admin/scans/stream".to_owned()
}
