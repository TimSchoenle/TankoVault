//! Public series browse/detail/chapters. Links are resolved to absolute URLs here via
//! `domain::resolve_link`; the database stays relative (design §11).

use crate::error::{ApiError, ApiResult};
use crate::state::AppState;
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue};
use axum_extra::extract::Query as MultiQuery;
use tankovault_db::repo::catalog::SeriesFilter;
use tankovault_domain::{
    ContentType, SeriesId, SeriesSourceId, SeriesStatus, UserId, resolve_link,
};
use serde::{Deserialize, Serialize};

/// Query parameters for the Discover browse list (frontend §9.1). All filters are optional;
/// `tag`/`exclude_tag` may repeat (`?tag=action&tag=drama`). Sorting and offset pagination
/// are server-side; the total match count + next page are returned as response headers so
/// the JSON body stays a plain array (backward-compatible with older clients).
#[derive(Debug, Deserialize)]
pub struct ListParams {
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub content_type: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    /// Provider slug the series must have a source on.
    #[serde(default)]
    pub provider: Option<String>,
    /// Tag slugs the series must all carry.
    #[serde(default)]
    pub tag: Vec<String>,
    /// Tag slugs the series must not carry.
    #[serde(default)]
    pub exclude_tag: Vec<String>,
    #[serde(default)]
    pub year_min: Option<i32>,
    #[serde(default)]
    pub year_max: Option<i32>,
    #[serde(default)]
    pub min_chapters: Option<i32>,
    /// `updated | title | chapters | sources | year` (default `updated`).
    #[serde(default)]
    pub sort: Option<String>,
    /// Zero-based page index (alias: `cursor`).
    #[serde(default)]
    pub page: Option<i64>,
    #[serde(default)]
    pub cursor: Option<i64>,
    #[serde(default = "default_limit")]
    pub limit: i64,
}

fn default_limit() -> i64 {
    40
}

#[derive(Debug, Serialize)]
pub struct SeriesSummary {
    pub id: SeriesId,
    pub title: String,
    pub cover_url: Option<String>,
    pub content_type: ContentType,
    pub status: SeriesStatus,
    pub source_count: i64,
}

/// `GET /v1/series` — filter/sort/paginate the browse list (frontend §9.1).
///
/// The body remains a plain `SeriesSummary[]`; pagination metadata rides on the
/// `X-Total-Count` (rows matching the filter) and `X-Next-Cursor` (next page index, absent
/// on the last page) headers so existing array-decoding clients keep working.
pub async fn list(
    State(state): State<AppState>,
    MultiQuery(params): MultiQuery<ListParams>,
) -> ApiResult<(HeaderMap, Json<Vec<SeriesSummary>>)> {
    let limit = params.limit.clamp(1, 100);
    let page = params.page.or(params.cursor).unwrap_or(0).max(0);
    let filter = SeriesFilter {
        query: params.query,
        content_type: params.content_type.filter(|s| !s.is_empty()),
        status: params.status.filter(|s| !s.is_empty()),
        provider_slug: params.provider.filter(|s| !s.is_empty()),
        tags: params.tag.into_iter().filter(|s| !s.is_empty()).collect(),
        exclude_tags: params
            .exclude_tag
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect(),
        year_min: params.year_min,
        year_max: params.year_max,
        min_chapters: params.min_chapters,
        sort: params.sort.filter(|s| !s.is_empty()),
        limit,
        offset: page * limit,
    };
    let out = tankovault_db::repo::catalog::list_series_filtered(&state.pool, &filter).await?;

    let mut headers = HeaderMap::new();
    if let Ok(v) = HeaderValue::from_str(&out.total.to_string()) {
        headers.insert("X-Total-Count", v);
    }
    let returned = i64::try_from(out.items.len()).unwrap_or(0);
    if filter.offset + returned < out.total {
        if let Ok(v) = HeaderValue::from_str(&(page + 1).to_string()) {
            headers.insert("X-Next-Cursor", v);
        }
    }

    let items = out
        .items
        .into_iter()
        .map(|it| SeriesSummary {
            id: it.series.id,
            title: it.series.canonical_title,
            cover_url: it.series.cover_url,
            content_type: it.series.content_type,
            status: it.series.status,
            source_count: it.source_count,
        })
        .collect();
    Ok((headers, Json(items)))
}

#[derive(Debug, Serialize)]
pub struct SourceDto {
    pub id: SeriesSourceId,
    pub provider_name: String,
    pub provider_slug: String,
    /// Resolved absolute URL to open the series on the provider.
    pub url: String,
    pub chapter_count: i32,
    /// True for the richest source (most chapters) — the one the reader should prefer
    /// (frontend §9.2). Exactly one source per series is flagged.
    pub is_primary: bool,
}

#[derive(Debug, Serialize)]
pub struct SeriesDetail {
    pub id: SeriesId,
    pub title: String,
    pub description: Option<String>,
    pub cover_url: Option<String>,
    pub content_type: ContentType,
    pub status: SeriesStatus,
    pub release_year: Option<i32>,
    pub sources: Vec<SourceDto>,
    /// Alternative titles gathered across providers (frontend §9.2; empty when none).
    pub alt_titles: Vec<String>,
    /// Genre/tags attached to the series (frontend §9.2; empty when none).
    pub tags: Vec<tankovault_domain::Tag>,
}

/// `GET /v1/series/:id` — canonical detail enriched with alt-titles, tags, and a primary
/// source flag (frontend §9.2). Rating/author are design-only (not in the domain) and are
/// deliberately omitted rather than fabricated.
pub async fn detail(
    State(state): State<AppState>,
    Path(id): Path<SeriesId>,
) -> ApiResult<Json<SeriesDetail>> {
    let series = tankovault_db::repo::catalog::get_series(&state.pool, id).await?;
    let sources = tankovault_db::repo::catalog::list_sources_for_series(&state.pool, id).await?;

    // The primary source is the one carrying the most chapters (ties → first listed).
    let primary_idx = sources
        .iter()
        .enumerate()
        .max_by_key(|(_, s)| s.chapter_count)
        .map(|(i, _)| i);

    let mut source_dtos = Vec::with_capacity(sources.len());
    for (i, src) in sources.into_iter().enumerate() {
        let provider = tankovault_db::repo::providers::get(&state.pool, src.provider_id).await?;
        let url =
            resolve_link(&provider.base_url, &src.source_path).map_err(|_| ApiError::Internal)?;
        source_dtos.push(SourceDto {
            id: src.id,
            provider_name: provider.name,
            provider_slug: provider.slug,
            url,
            chapter_count: src.chapter_count,
            is_primary: Some(i) == primary_idx,
        });
    }

    let alt_titles = tankovault_db::repo::catalog::list_series_titles(&state.pool, id).await?;
    let tags = tankovault_db::repo::catalog::list_series_tags(&state.pool, id).await?;

    Ok(Json(SeriesDetail {
        id: series.id,
        title: series.canonical_title,
        description: series.description,
        cover_url: series.cover_url,
        content_type: series.content_type,
        status: series.status,
        release_year: series.release_year,
        sources: source_dtos,
        alt_titles,
        tags,
    }))
}

#[derive(Debug, Deserialize)]
pub struct ChapterParams {
    /// Which source to read chapters from. Defaults to the first source of the series.
    pub source: Option<SeriesSourceId>,
}

#[derive(Debug, Serialize)]
pub struct ChapterDto {
    pub number: f64,
    pub title: Option<String>,
    /// Resolved absolute URL to open the chapter page on the provider.
    pub url: String,
    #[serde(with = "time::serde::rfc3339::option")]
    pub published_at: Option<time::OffsetDateTime>,
    /// Whether the requesting user has read this chapter (number ≤ their progress).
    /// `None` for anonymous requests; `Some(bool)` when a valid `Bearer` token is present
    /// (frontend §9.2 auth-scoped read-state).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read: Option<bool>,
}

/// `GET /v1/series/:id/chapters?source=` — chapter list, newest first. When a valid access
/// token is supplied the per-chapter `read` flag is populated from the user's progress
/// (frontend §9.2); anonymous callers get the same list without read-state.
pub async fn chapters(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<SeriesId>,
    Query(params): Query<ChapterParams>,
) -> ApiResult<Json<Vec<ChapterDto>>> {
    let source_id = if let Some(s) = params.source {
        s
    } else {
        let sources = tankovault_db::repo::catalog::list_sources_for_series(&state.pool, id).await?;
        sources.first().map(|s| s.id).ok_or(ApiError::NotFound)?
    };

    let (_, base_url) =
        tankovault_db::repo::catalog::source_provider_base_url(&state.pool, source_id).await?;
    let chapters = tankovault_db::repo::catalog::list_chapters(&state.pool, source_id).await?;

    // Read-state is opt-in: only when a valid token identifies the user.
    let progress = match optional_user(&state, &headers) {
        Some(user_id) => {
            tankovault_db::repo::tracking::progress_get(&state.pool, user_id, id).await?
        }
        None => None,
    };

    let out = chapters
        .into_iter()
        .map(|c| {
            Ok(ChapterDto {
                number: c.number,
                title: c.title,
                url: resolve_link(&base_url, &c.path).map_err(|_| ApiError::Internal)?,
                published_at: c.published_at,
                read: progress.map(|last| c.number <= last),
            })
        })
        .collect::<ApiResult<Vec<_>>>()?;
    Ok(Json(out))
}

/// Best-effort user extraction for endpoints that are public but *enrich* their response
/// when authenticated. Returns `None` for a missing/invalid token instead of rejecting the
/// request, so anonymous browsing keeps working.
fn optional_user(state: &AppState, headers: &HeaderMap) -> Option<UserId> {
    let token = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))?;
    tankovault_auth::verify_access_token(&state.jwt_secret, token)
        .ok()
        .and_then(|c| c.user_id())
}

/// `GET /v1/tags` — all genres/tags (public).
pub async fn tags(State(state): State<AppState>) -> ApiResult<Json<Vec<tankovault_domain::Tag>>> {
    Ok(Json(
        tankovault_db::repo::catalog::list_tags(&state.pool).await?,
    ))
}

/// `GET /v1/providers` — public provider list + per-provider series counts, for the Discover
/// provider filter (frontend §9.3). Operator-only fields (config/politeness/health) are not
/// exposed; disabled providers are hidden.
pub async fn providers(
    State(state): State<AppState>,
) -> ApiResult<Json<Vec<tankovault_db::repo::providers::PublicProvider>>> {
    Ok(Json(
        tankovault_db::repo::providers::list_public(&state.pool).await?,
    ))
}
