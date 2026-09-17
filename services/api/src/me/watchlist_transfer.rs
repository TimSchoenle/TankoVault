//! Watchlist backup and restore: a portable export of the reader's watchlist, an import that
//! merges one back in on any deployment, and the queue of imported entries still waiting for
//! their series.
//!
//! The document names series by provider page, external tracker id and title, never by uuid, so
//! a backup restores into a wiped database or a different deployment. See
//! [`tankovault_contracts::watchlist_transfer`] for the format and its versioning.

use super::progress::spawn_targeted_push_many;
use crate::audit::audit;
use crate::content_gate::AdultVisibility;
use crate::error::{ApiError, ApiResult};
use crate::openapi::ME_WATCHLIST_TAG;
use crate::state::{AppState, AuthUser};
use crate::step_up::Elevated;
use axum::Json;
use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use tankovault_contracts::watchlist_transfer::{
    DocumentError, ISSUE_LIST_LIMIT, ImportIssue, ImportMode, ImportReport, PENDING_LIST_LIMIT,
    PendingEntryView, PendingView, export_document, parse_document,
};
use tankovault_db::repo::tracking::{BULK_ID_LIMIT, Commit, ImportOptions, ImportRefusal};
use tankovault_domain::SeriesId;
use tankovault_domain::watchlist_transfer::{ConflictPolicy, PortableEntry};
use time::OffsetDateTime;
use utoipa::IntoParams;

/// The largest import body accepted, in place of the service-wide `security.max_body_bytes`.
///
/// A backup of [`tankovault_domain::watchlist_transfer::MAX_ENTRIES`] entries runs to a few MiB
/// pretty-printed, past the 1 MiB default every other route keeps. The raise is scoped to the two
/// import routes, which are also behind the `expensive` rate-limit budget, and the entry and
/// field limits bound what a body this size can make the server do.
pub const IMPORT_BODY_LIMIT: usize = 8 * 1024 * 1024;

/// How an import should treat the watchlist it lands in.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct WatchlistImportParams {
    /// `merge` (default) | `overwrite` | `replace`.
    #[serde(default)]
    pub mode: ImportMode,
    /// Also match an entry by a title naming exactly one series, when no provider page or
    /// external id matches. Off by default: two different works share titles often enough that
    /// this is the reader's call.
    #[serde(default)]
    pub match_titles: bool,
}

/// Export the watchlist
///
/// The reader's whole watchlist — statuses, notification and sync choices, read progress, pinned
/// sources — as a versioned JSON document that identifies each series without database ids, so
/// it imports into a wiped database or another deployment.
///
/// Not behind a step-up: every value in it is already readable page by page through
/// `GET /v1/me/watchlist`, so elevation here would inconvenience the owner without denying a
/// stolen session anything.
#[utoipa::path(
    get,
    path = "/v1/me/watchlist/export",
    tag = ME_WATCHLIST_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "The watchlist backup", body = tankovault_contracts::watchlist_transfer::v1::Document),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn export_watchlist(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Response> {
    let entries =
        tankovault_db::repo::tracking::watchlist_export(&state.pool, user.user_id).await?;
    let count = entries.len();
    let now = OffsetDateTime::now_utc();
    let document = export_document(now, entries);

    audit(
        &state,
        &user,
        "watchlist.export",
        &user.user_id.as_uuid().to_string(),
        &serde_json::json!({ "entries": count }),
    )
    .await;

    let mut response = Json(document).into_response();
    let headers = response.headers_mut();
    let filename = format!(
        "tankovault-watchlist-{:04}-{:02}-{:02}.json",
        now.year(),
        u8::from(now.month()),
        now.day()
    );
    if let Ok(value) = HeaderValue::from_str(&format!("attachment; filename=\"{filename}\"")) {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }
    // A reading history must not be kept by a shared cache or the browser's back-forward cache.
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

/// Refuse anything but a JSON body before parsing it.
///
/// The bearer token already rules out a cross-site form post, so this is about an honest client
/// sending the wrong file: a `400` naming the problem, rather than a serde error about line 1.
fn require_json(headers: &HeaderMap) -> ApiResult<()> {
    let is_json = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("application/json"));
    if is_json {
        Ok(())
    } else {
        Err(ApiError::BadRequest(
            "a watchlist backup must be sent as application/json".into(),
        ))
    }
}

fn parse(headers: &HeaderMap, body: &Bytes) -> ApiResult<(u32, Vec<PortableEntry>)> {
    require_json(headers)?;
    let document =
        parse_document(body).map_err(|e: DocumentError| ApiError::BadRequest(e.to_string()))?;
    Ok((document.version, document.entries))
}

const fn options(params: &WatchlistImportParams, adult: AdultVisibility) -> ImportOptions {
    ImportOptions {
        policy: match params.mode {
            ImportMode::Merge => ConflictPolicy::KeepLocal,
            ImportMode::Overwrite | ImportMode::Replace => ConflictPolicy::PreferImported,
        },
        remove_missing: matches!(params.mode, ImportMode::Replace),
        match_titles: params.match_titles,
        include_adult: adult.include_adult(),
    }
}

fn issues(entries: &[PortableEntry], indices: &[usize]) -> Vec<ImportIssue> {
    indices
        .iter()
        .take(ISSUE_LIST_LIMIT)
        .map(|&i| ImportIssue {
            index: i64::try_from(i).unwrap_or(i64::MAX),
            title: entries[i].title.clone(),
        })
        .collect()
}

fn count(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

/// Run an import, or its dry run, and describe the outcome.
async fn run(
    state: &AppState,
    user: &AuthUser,
    params: &WatchlistImportParams,
    adult: AdultVisibility,
    version: u32,
    entries: &[PortableEntry],
    commit: Commit,
) -> ApiResult<(ImportReport, Vec<SeriesId>)> {
    let outcome = tankovault_db::repo::tracking::watchlist_import(
        &state.pool,
        user.user_id,
        entries,
        options(params, adult),
        commit,
    )
    .await?
    .map_err(|refusal| match refusal {
        ImportRefusal::PendingLimit => ApiError::Conflict(format!(
            "this import would leave more than {} entries waiting for their series; discard \
             pending entries or import in `replace` mode",
            tankovault_domain::watchlist_transfer::MAX_ENTRIES
        )),
    })?;

    let plan = &outcome.plan;
    let mut touched: Vec<SeriesId> = plan.writes.iter().map(|w| w.series_id).collect();
    touched.extend(plan.progress.iter().map(|(id, _)| *id));
    touched.sort_unstable_by_key(|id| id.as_uuid());
    touched.dedup();

    let report = ImportReport {
        dry_run: commit == Commit::DryRun,
        mode: params.mode,
        format_version: version,
        entries: count(entries.len()),
        added: count(plan.added),
        updated: count(plan.updated),
        unchanged: count(plan.unchanged),
        removed: count(outcome.removed),
        progress_written: count(plan.progress.len()),
        pending: count(plan.unmatched.len()),
        duplicates: count(plan.duplicates.len()),
        pending_entries: issues(entries, &plan.unmatched),
        title_matches: issues(entries, &plan.title_matches),
    };
    Ok((report, touched))
}

/// Preview a watchlist import
///
/// Everything [`import_watchlist`] would do with this document — what it adds, changes, removes
/// and queues — computed against the current watchlist and then rolled back. Nothing is written.
///
/// A body over 8 MiB is refused with `413` before it is read.
#[utoipa::path(
    post,
    path = "/v1/me/watchlist/import/preview",
    tag = ME_WATCHLIST_TAG,
    params(WatchlistImportParams),
    request_body(content = serde_json::Value, content_type = "application/json", description = "A watchlist backup, any supported version"),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "What the import would do", body = ImportReport),
        (status = 400, description = "not a valid watchlist backup, or an unsupported version", body = crate::error::ProblemDetails),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 409, description = "the import would exceed the pending-entry limit", body = crate::error::ProblemDetails),
    )
)]
pub async fn preview_watchlist_import(
    State(state): State<AppState>,
    user: AuthUser,
    adult: AdultVisibility,
    Query(params): Query<WatchlistImportParams>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Json<ImportReport>> {
    let (version, entries) = parse(&headers, &body)?;
    let (report, _) = run(
        &state,
        &user,
        &params,
        adult,
        version,
        &entries,
        Commit::DryRun,
    )
    .await?;
    Ok(Json(report))
}

/// Import a watchlist backup
///
/// Applies a backup in one transaction. Entries whose series this deployment has not crawled yet
/// are queued rather than dropped, and attach automatically once a scan brings the series in;
/// see `GET /v1/me/watchlist/import/pending`.
///
/// Behind a step-up. `replace` removes every entry the backup lacks and `overwrite` rewrites
/// every one it names, so a single call can undo a reader's whole library.
///
/// A body over 8 MiB is refused with `413` before it is read.
#[utoipa::path(
    post,
    path = "/v1/me/watchlist/import",
    tag = ME_WATCHLIST_TAG,
    params(WatchlistImportParams),
    request_body(content = serde_json::Value, content_type = "application/json", description = "A watchlist backup, any supported version"),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "What the import did", body = ImportReport),
        (status = 400, description = "not a valid watchlist backup, or an unsupported version", body = crate::error::ProblemDetails),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "a step-up is required", body = crate::error::ProblemDetails),
        (status = 409, description = "the import would exceed the pending-entry limit", body = crate::error::ProblemDetails),
    )
)]
pub async fn import_watchlist(
    State(state): State<AppState>,
    Elevated(user): Elevated,
    adult: AdultVisibility,
    Query(params): Query<WatchlistImportParams>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Json<ImportReport>> {
    let (version, entries) = parse(&headers, &body)?;
    let (report, touched) = run(
        &state,
        &user,
        &params,
        adult,
        version,
        &entries,
        Commit::Apply,
    )
    .await?;

    audit(
        &state,
        &user,
        "watchlist.import",
        &user.user_id.as_uuid().to_string(),
        &serde_json::json!({
            "mode": params.mode,
            "match_titles": params.match_titles,
            "format_version": version,
            "entries": report.entries,
            "added": report.added,
            "updated": report.updated,
            "removed": report.removed,
            "pending": report.pending,
        }),
    )
    .await;

    // Reflected to linked trackers the way a bulk edit is, up to the same bound; past it the
    // periodic reconciliation carries the change, rather than one import queueing thousands of
    // third-party writes behind each other.
    if touched.len() <= BULK_ID_LIMIT {
        spawn_targeted_push_many(&state, user.user_id, touched);
    }
    Ok(Json(report))
}

/// List pending import entries
///
/// Imported entries whose series this deployment has not crawled yet. Each attaches on its own
/// once a scan brings its series in; this is what is still waiting.
#[utoipa::path(
    get,
    path = "/v1/me/watchlist/import/pending",
    tag = ME_WATCHLIST_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "The caller's pending entries, oldest first", body = PendingView),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn watchlist_import_pending(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<PendingView>> {
    let (total, items) = tankovault_db::repo::tracking::watchlist_pending(
        &state.pool,
        user.user_id,
        PENDING_LIST_LIMIT,
    )
    .await?;
    Ok(Json(PendingView {
        total,
        items: items
            .into_iter()
            .map(|p| PendingEntryView {
                title: p.title,
                source_urls: p.source_urls,
                queued_at: p.queued_at,
                last_attempt_at: p.last_attempt_at,
                attempts: p.attempts,
            })
            .collect(),
    }))
}

/// Discard pending import entries
///
/// Stops waiting for every pending entry. Idempotent; entries already attached are unaffected.
#[utoipa::path(
    delete,
    path = "/v1/me/watchlist/import/pending",
    tag = ME_WATCHLIST_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 204, description = "Discarded"),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn clear_watchlist_import_pending(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<StatusCode> {
    let discarded =
        tankovault_db::repo::tracking::watchlist_pending_clear(&state.pool, user.user_id).await?;
    if discarded > 0 {
        audit(
            &state,
            &user,
            "watchlist.import.pending.clear",
            &user.user_id.as_uuid().to_string(),
            &serde_json::json!({ "discarded": discarded }),
        )
        .await;
    }
    Ok(StatusCode::NO_CONTENT)
}
