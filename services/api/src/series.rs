//! Public series browse/detail/chapters. Links are resolved to absolute URLs here via
//! `domain::resolve_link`; the database stays relative (design §11).

use crate::error::{ApiError, ApiResult};
use crate::state::AppState;
use axum::Json;
use axum::extract::{Path, Query, State};
use tankovault_domain::{ContentType, SeriesId, SeriesSourceId, SeriesStatus, resolve_link};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct ListParams {
    #[serde(default)]
    pub query: Option<String>,
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

/// `GET /v1/series`
pub async fn list(
    State(state): State<AppState>,
    Query(params): Query<ListParams>,
) -> ApiResult<Json<Vec<SeriesSummary>>> {
    let limit = params.limit.clamp(1, 100);
    let query = params.query.as_deref().filter(|q| !q.trim().is_empty());
    let items = tankovault_db::repo::catalog::list_series(&state.pool, query, limit).await?;
    let out = items
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
    Ok(Json(out))
}

#[derive(Debug, Serialize)]
pub struct SourceDto {
    pub id: SeriesSourceId,
    pub provider_name: String,
    pub provider_slug: String,
    /// Resolved absolute URL to open the series on the provider.
    pub url: String,
    pub chapter_count: i32,
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
}

/// `GET /v1/series/:id`
pub async fn detail(
    State(state): State<AppState>,
    Path(id): Path<SeriesId>,
) -> ApiResult<Json<SeriesDetail>> {
    let series = tankovault_db::repo::catalog::get_series(&state.pool, id).await?;
    let sources = tankovault_db::repo::catalog::list_sources_for_series(&state.pool, id).await?;

    let mut source_dtos = Vec::with_capacity(sources.len());
    for src in sources {
        let provider = tankovault_db::repo::providers::get(&state.pool, src.provider_id).await?;
        let url =
            resolve_link(&provider.base_url, &src.source_path).map_err(|_| ApiError::Internal)?;
        source_dtos.push(SourceDto {
            id: src.id,
            provider_name: provider.name,
            provider_slug: provider.slug,
            url,
            chapter_count: src.chapter_count,
        });
    }

    Ok(Json(SeriesDetail {
        id: series.id,
        title: series.canonical_title,
        description: series.description,
        cover_url: series.cover_url,
        content_type: series.content_type,
        status: series.status,
        release_year: series.release_year,
        sources: source_dtos,
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
}

/// `GET /v1/series/:id/chapters?source=`
pub async fn chapters(
    State(state): State<AppState>,
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

    let out = chapters
        .into_iter()
        .map(|c| {
            Ok(ChapterDto {
                number: c.number,
                title: c.title,
                url: resolve_link(&base_url, &c.path).map_err(|_| ApiError::Internal)?,
                published_at: c.published_at,
            })
        })
        .collect::<ApiResult<Vec<_>>>()?;
    Ok(Json(out))
}

/// `GET /v1/tags` — all genres/tags (public).
pub async fn tags(State(state): State<AppState>) -> ApiResult<Json<Vec<tankovault_domain::Tag>>> {
    Ok(Json(
        tankovault_db::repo::catalog::list_tags(&state.pool).await?,
    ))
}
