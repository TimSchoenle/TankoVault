//! Catalogue maintenance: the operator's series list, the deployment totals behind the purge
//! panel's stated blast radius, bulk deletion, and the purge itself.
//!
//! Deleting a series is not the same act as merging one. A merge folds a duplicate into a
//! survivor and carries every reader's watchlist entry and reading position across; these
//! endpoints discard both. That is why they sit behind their own capability rather than
//! `merge.write`, and why every response states what actually went.
//!
//! The listing is **not** narrowed by the adult-content gate, unlike every reader-facing read.
//! That gate decides what a *reader* is shown; an operator who cannot see an adult-classified row
//! cannot delete it either, which would make the one surface for removing unwanted material blind
//! to exactly the material most likely to need removing.

use crate::audit::{audit, audit_failure};
use crate::error::{ApiError, ApiResult};
use crate::openapi::ADMIN_CATALOGUE_TAG;
use crate::state::{AppState, AuthUser};
use axum::Json;
use axum::extract::{Query, State};
use serde::{Deserialize, Serialize};
use tankovault_db::repo::catalog::maintenance::{
    self, DeletionReport, MaintenanceFilter, SeriesHealth,
};
use tankovault_domain::{Permission, SeriesId};
use time::OffsetDateTime;
use utoipa::{IntoParams, ToSchema};

/// Page size cap. The console offers 25/50/100; a caller asking for everything would page the
/// whole catalogue into one response.
const MAX_PAGE: i64 = 200;

/// How many series a single bulk delete may name.
///
/// Bounded because each id cascades into a dozen tables, and one request has to finish inside
/// the request timeout. Emptying the deployment is what [`purge_catalogue`] is for.
const MAX_BULK_DELETE: usize = 500;

/// Series removed per purge *batch*. See [`maintenance::purge_series_batch`] for why the purge
/// is batched at all, and [`PURGE_BUDGET`] for how many batches one call runs.
const PURGE_SERIES_BATCH: i64 = 500;

/// Chapters removed per purge batch. Higher than the series batch because a chapter row cascades
/// into nothing.
const PURGE_CHAPTER_BATCH: i64 = 20_000;

/// How long one purge call keeps running batches before it answers.
///
/// A batch is the unit the *database* can commit; it is not the unit a client should have to
/// call. At 500 series a call, emptying a 50 000-series catalogue took a hundred requests, and
/// this route draws on the tight write budget (`crate::route_classifier`) — ten of which is a
/// burst, thirty a minute sustained. So the panel spent its budget in seconds and every call
/// after that was a `429`: the purge could not finish, on any catalogue large enough to need one.
///
/// Draining to a deadline instead keeps every property the batching exists for — each batch
/// commits, an interrupted purge leaves a smaller catalogue, the caller still repeats until
/// `done` — while cutting the call count by two orders of magnitude. Well inside the 30 s
/// request timeout, with room for the slowest batch to overrun it.
const PURGE_BUDGET: std::time::Duration = std::time::Duration::from_secs(10);

/// Which rows the maintenance list is narrowed to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum HealthFilter {
    #[default]
    Any,
    /// No provider carries this series any more, so no scan will ever touch it again.
    Orphaned,
    /// Carried by a provider, but no chapter has ever been discovered.
    Empty,
}

impl From<HealthFilter> for SeriesHealth {
    fn from(filter: HealthFilter) -> Self {
        match filter {
            HealthFilter::Any => Self::Any,
            HealthFilter::Orphaned => Self::Orphaned,
            HealthFilter::Empty => Self::Empty,
        }
    }
}

#[derive(Debug, Default, Deserialize, IntoParams)]
pub struct MaintenanceQuery {
    /// Case-insensitive substring of the canonical title. Empty lists everything.
    #[serde(default)]
    pub search: Option<String>,
    /// Restrict to series carried by this provider slug.
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub health: Option<HealthFilter>,
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub offset: Option<i64>,
}

/// One row of the maintenance list.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = CatalogueRow)]
pub struct CatalogueRowView {
    pub id: SeriesId,
    pub title: String,
    /// Content-type token (`manga` | `manhwa` | …).
    pub content_type: String,
    /// Publication-status token (`ongoing` | `completed` | …).
    pub status: String,
    pub release_year: Option<i32>,
    /// Slugs of every provider carrying it, so the operator can see what a deletion would have
    /// to be re-scanned from.
    pub providers: Vec<String>,
    pub source_count: i64,
    pub chapter_count: i64,
    /// Readers with this series on a watchlist — the part of the blast radius no re-scan
    /// restores.
    pub watcher_count: i64,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub updated_at: OffsetDateTime,
}

/// A page of the maintenance list plus the total matching the filter, so the pager needs no
/// second request.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = CataloguePage)]
pub struct CataloguePageView {
    pub items: Vec<CatalogueRowView>,
    pub total: i64,
}

/// Deployment-wide catalogue totals.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = CatalogueSummary)]
pub struct CatalogueSummaryView {
    pub series_total: i64,
    pub sources_total: i64,
    pub chapters_total: i64,
    pub orphaned_series: i64,
    pub empty_series: i64,
    /// Watchlist entries a full purge would take with it.
    pub watchlist_entries: i64,
    /// Reading positions a full purge would take with it.
    pub progress_rows: i64,
}

/// What a deletion actually removed, per table, counted rather than estimated.
#[derive(Debug, Clone, Copy, Default, Serialize, ToSchema)]
#[schema(as = CatalogueDeletion)]
pub struct DeletionView {
    pub series: i64,
    pub sources: i64,
    pub chapters: i64,
    pub watchlist_entries: i64,
    pub progress_rows: i64,
}

impl DeletionView {
    /// Fold another batch's counts in, so one call's report covers everything it removed.
    fn add(&mut self, other: Self) {
        self.series += other.series;
        self.sources += other.sources;
        self.chapters += other.chapters;
        self.watchlist_entries += other.watchlist_entries;
        self.progress_rows += other.progress_rows;
    }

    /// Whether this batch removed anything of the purged kind.
    ///
    /// The loop's stop condition when the count of what is left refuses to fall: a batch that
    /// deletes nothing while `remaining` stays positive would otherwise spin against the
    /// database for the whole budget. Reaching it means something is wrong — a row the delete
    /// cannot reach — and answering with the counts on screen is the honest outcome.
    const fn is_empty(&self) -> bool {
        self.series == 0 && self.chapters == 0
    }
}

impl From<DeletionReport> for DeletionView {
    fn from(report: DeletionReport) -> Self {
        Self {
            series: report.series,
            sources: report.sources,
            chapters: report.chapters,
            watchlist_entries: report.watchlist_entries,
            progress_rows: report.progress_rows,
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct BulkDeleteSeries {
    /// The series to remove. Ids naming nothing are skipped rather than failing the batch.
    pub series_ids: Vec<SeriesId>,
}

/// How much of the catalogue a purge takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PurgeScope {
    /// Every chapter. Series and their provider sources survive, so the next scan refills them.
    Chapters,
    /// Every series, and with it every source, chapter, watchlist entry and reading position
    /// that hung off one.
    Everything,
}

impl PurgeScope {
    /// The word the caller has to echo back. Its own token, so a request that names one scope
    /// and confirms another is refused rather than resolved in either direction.
    fn token(self) -> &'static str {
        match self {
            Self::Chapters => "chapters",
            Self::Everything => "everything",
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct PurgeRequest {
    pub scope: PurgeScope,
    /// The scope's own token, echoed back. Same guard as `confirm_username` on the account
    /// erasure paths: it is what stops a mis-aimed script from emptying a deployment on a
    /// request body it built by accident.
    pub confirm: String,
}

/// One purge call's progress.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = CataloguePurge)]
pub struct PurgeView {
    pub scope: PurgeScope,
    pub removed: DeletionView,
    /// Rows of the purged kind still standing. The caller repeats until this is zero.
    pub remaining: i64,
    /// Whether this call finished the job.
    pub done: bool,
}

/// List the catalogue for maintenance
///
/// The operator's view of the catalogue: what is in it, how much of it each series is, and how
/// many readers would notice it going. Newest first, filterable by title, provider and health.
#[utoipa::path(
    get,
    path = "/v1/admin/catalogue/series",
    tag = ADMIN_CATALOGUE_TAG,
    params(MaintenanceQuery),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "A page of the catalogue", body = CataloguePageView),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "no second factor is enrolled, or the caller does not hold the required permission", body = crate::error::ProblemDetails),
    )
)]
pub async fn list_catalogue(
    State(state): State<AppState>,
    user: AuthUser,
    Query(q): Query<MaintenanceQuery>,
) -> ApiResult<Json<CataloguePageView>> {
    user.require(Permission::CatalogueRead).await?;
    let filter = MaintenanceFilter {
        search: q.search.unwrap_or_default().trim().to_owned(),
        provider_slug: q
            .provider
            .map(|slug| slug.trim().to_owned())
            .filter(|slug| !slug.is_empty()),
        health: q.health.unwrap_or_default().into(),
        limit: q.limit.unwrap_or(50).clamp(1, MAX_PAGE),
        offset: q.offset.unwrap_or(0).max(0),
    };
    let page = maintenance::list_for_maintenance(&state.pool, &filter).await?;
    Ok(Json(CataloguePageView {
        total: page.total,
        items: page
            .items
            .into_iter()
            .map(|row| CatalogueRowView {
                id: SeriesId::from_uuid(row.id),
                title: row.canonical_title,
                content_type: row.content_type,
                status: row.status,
                release_year: row.release_year,
                providers: row.providers,
                source_count: row.source_count,
                chapter_count: row.chapter_count,
                watcher_count: row.watcher_count,
                created_at: row.created_at,
                updated_at: row.updated_at,
            })
            .collect(),
    }))
}

/// Summarise the catalogue
///
/// The totals the purge panel states its blast radius from, including the two counts that say
/// how much of the catalogue is junk: series no provider carries, and series no chapter was ever
/// found for.
#[utoipa::path(
    get,
    path = "/v1/admin/catalogue/summary",
    tag = ADMIN_CATALOGUE_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Deployment-wide catalogue totals", body = CatalogueSummaryView),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "no second factor is enrolled, or the caller does not hold the required permission", body = crate::error::ProblemDetails),
    )
)]
pub async fn catalogue_summary(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<CatalogueSummaryView>> {
    user.require(Permission::CatalogueRead).await?;
    let totals = maintenance::totals(&state.pool).await?;
    Ok(Json(CatalogueSummaryView {
        series_total: totals.series_total,
        sources_total: totals.sources_total,
        chapters_total: totals.chapters_total,
        orphaned_series: totals.orphaned_series,
        empty_series: totals.empty_series,
        watchlist_entries: totals.watchlist_entries,
        progress_rows: totals.progress_rows,
    }))
}

/// Delete series in bulk
///
/// Removes every named series and everything the schema hangs off it — sources, chapters, tag
/// links, watchlist entries and reading positions. One transaction, so the batch either applies
/// or does not.
#[utoipa::path(
    post,
    path = "/v1/admin/catalogue/series/delete",
    tag = ADMIN_CATALOGUE_TAG,
    request_body = BulkDeleteSeries,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "What was removed", body = DeletionView),
        (status = 400, description = "no ids, or more than the per-request cap", body = crate::error::ProblemDetails),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "no second factor is enrolled, a step-up is required, or the caller does not hold the required permission", body = crate::error::ProblemDetails),
    )
)]
pub async fn bulk_delete_series(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<BulkDeleteSeries>,
) -> ApiResult<Json<DeletionView>> {
    user.require(Permission::CatalogueDelete).await?;

    if body.series_ids.is_empty() {
        return Err(ApiError::BadRequest("name at least one series".to_owned()));
    }
    if body.series_ids.len() > MAX_BULK_DELETE {
        return Err(ApiError::BadRequest(format!(
            "at most {MAX_BULK_DELETE} series per request; use the purge to empty the catalogue"
        )));
    }

    let ids: Vec<uuid::Uuid> = body.series_ids.iter().map(|id| id.as_uuid()).collect();
    let mut conn = state.pool.acquire().await.map_err(|e| {
        tracing::error!(error = %e, "failed to acquire a connection for a bulk series delete");
        ApiError::Internal
    })?;
    let report = maintenance::delete_series(&mut conn, &ids).await?;
    drop(conn);

    audit(
        &state,
        &user,
        "catalogue.series.delete",
        "-",
        // The ids, not just the count: a bulk delete is the one action whose audit record has
        // to answer "which ones", and the cap above bounds how long this can get.
        &serde_json::json!({
            "series_ids": body.series_ids,
            "series_deleted": report.series,
            "chapters_deleted": report.chapters,
            "watchlist_entries_deleted": report.watchlist_entries,
        }),
    )
    .await;

    Ok(Json(report.into()))
}

/// Purge the catalogue
///
/// Empties the catalogue for up to ten seconds per call. The response says how much this call
/// removed and how much is left, and the caller repeats until `done`.
///
/// # Why this is not one request
///
/// A full catalogue cascades into a dozen tables and takes minutes, far longer than the request
/// timeout allows. A single statement would therefore be killed and rolled back every time, and
/// the deployment could never actually be emptied. Batching makes the operation resumable
/// instead: each batch commits, and an interrupted purge leaves a smaller catalogue rather than
/// no progress at all.
///
/// # Why it is not one batch per request either
///
/// See [`PURGE_BUDGET`]: a call per batch spent the caller's rate-limit budget long before the
/// catalogue was empty.
#[utoipa::path(
    post,
    path = "/v1/admin/catalogue/purge",
    tag = ADMIN_CATALOGUE_TAG,
    request_body = PurgeRequest,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "What this batch removed and what is left", body = PurgeView),
        (status = 400, description = "`confirm` does not echo the scope", body = crate::error::ProblemDetails),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "no second factor is enrolled, a step-up is required, or the caller does not hold the required permission", body = crate::error::ProblemDetails),
    )
)]
pub async fn purge_catalogue(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<PurgeRequest>,
) -> ApiResult<Json<PurgeView>> {
    user.require(Permission::CatalogueDelete).await?;

    if body.confirm.trim() != body.scope.token() {
        audit_failure(
            &state,
            &user,
            "catalogue.purge",
            body.scope.token(),
            &serde_json::json!({ "reason": "confirmation_mismatch" }),
        )
        .await;
        return Err(ApiError::BadRequest(format!(
            "confirm must be {:?} to purge that scope",
            body.scope.token()
        )));
    }

    let mut conn = state.pool.acquire().await.map_err(|e| {
        tracing::error!(error = %e, "failed to acquire a connection for a catalogue purge");
        ApiError::Internal
    })?;
    let started = std::time::Instant::now();
    let mut removed = DeletionView::default();
    let mut remaining;
    loop {
        let (report, left) = match body.scope {
            PurgeScope::Chapters => {
                maintenance::purge_chapters_batch(&mut conn, PURGE_CHAPTER_BATCH).await?
            }
            PurgeScope::Everything => {
                maintenance::purge_series_batch(&mut conn, PURGE_SERIES_BATCH).await?
            }
        };
        let batch = DeletionView::from(report);
        let stalled = batch.is_empty();
        removed.add(batch);
        remaining = left;
        if left == 0 || stalled || started.elapsed() >= PURGE_BUDGET {
            break;
        }
    }
    drop(conn);

    let view = PurgeView {
        scope: body.scope,
        removed,
        remaining,
        done: remaining == 0,
    };
    // Every call is audited, not just the first. Each one is a separate authorized destructive
    // act, and a trail that recorded only the opening call could not answer how far a purge
    // someone interrupted actually got.
    audit(
        &state,
        &user,
        "catalogue.purge",
        body.scope.token(),
        &serde_json::to_value(&view).unwrap_or_default(),
    )
    .await;
    Ok(Json(view))
}
