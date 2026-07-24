//! Public series browse/detail/chapters. Links are resolved to absolute URLs here via
//! `domain::resolve_link`; the database stays relative (design §11).

use crate::error::{ApiError, ApiResult};
use crate::state::AppState;
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue};
use axum_extra::extract::Query as MultiQuery;
use serde::{Deserialize, Serialize};
use tankovault_db::repo::catalog::SeriesFilter;
use tankovault_domain::{
    ContentType, ProviderId, SeriesId, SeriesSource, SeriesSourceId, SeriesStatus, UserId,
    resolve_link,
};
use utoipa::{IntoParams, ToSchema};

use crate::openapi::SERIES_TAG;

/// Query parameters for the Discover browse list (frontend §9.1). All filters are optional;
/// `tag`/`exclude_tag` may repeat (`?tag=action&tag=drama`). Sorting and offset pagination
/// are server-side; the total match count + next page are returned as response headers so
/// the JSON body stays a plain array (backward-compatible with older clients).
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
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

#[derive(Debug, Serialize, ToSchema)]
pub struct SeriesSummary {
    pub id: SeriesId,
    pub title: String,
    pub cover_url: Option<String>,
    pub content_type: ContentType,
    pub status: SeriesStatus,
    pub source_count: i64,
}

/// Browse the catalogue
///
/// Filter/sort/paginate the public series list (frontend §9.1). The body remains a plain
/// `SeriesSummary[]`; pagination metadata rides on the `X-Total-Count` (rows matching the
/// filter) and `X-Next-Cursor` (next page index, absent on the last page) headers so existing
/// array-decoding clients keep working.
#[utoipa::path(
    get,
    path = "/v1/series",
    tag = SERIES_TAG,
    params(ListParams),
    responses(
        (
            status = 200, description = "Matching series, newest-updated first by default",
            body = Vec<SeriesSummary>,
            headers(
                ("X-Total-Count" = i64, description = "Total rows matching the filter"),
                ("X-Next-Cursor" = i64, description = "Next page index; absent on the last page"),
            ),
        ),
    )
)]
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

#[derive(Debug, Serialize, ToSchema)]
pub struct SourceDto {
    pub id: SeriesSourceId,
    pub provider_name: String,
    pub provider_slug: String,
    /// Resolved absolute URL to open the series on the provider.
    pub url: String,
    /// Count of distinct **whole** chapters (`floor(number)`-deduped) — sub-chapter part
    /// releases don't inflate this (§ chapter grouping).
    pub chapter_count: i32,
    /// True for the richest source (most chapters) — the one the reader should prefer
    /// (frontend §9.2). Exactly one source per series is flagged.
    pub is_primary: bool,
}

#[derive(Debug, Serialize, ToSchema)]
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
    /// Author/artist credits attached to the series; empty when none.
    pub authors: Vec<tankovault_domain::Author>,
    /// `AniList` media id, if this series is mapped (`sync_mappings`); lets the frontend
    /// link out to the canonical `AniList` entry regardless of whether the viewer has synced.
    pub anilist_id: Option<String>,
}

/// Get series detail
#[utoipa::path(
    get,
    path = "/v1/series/{id}",
    tag = SERIES_TAG,
    params(("id" = SeriesId, Path, description = "Series id")),
    responses(
        (status = 200, description = "Series detail", body = SeriesDetail),
        (status = 404, description = "Series not found", body = crate::error::ProblemDetails),
    )
)]
pub async fn detail(
    State(state): State<AppState>,
    Path(id): Path<SeriesId>,
) -> ApiResult<Json<SeriesDetail>> {
    let series = tankovault_db::repo::catalog::get_series(&state.pool, id).await?;
    let sources = tankovault_db::repo::catalog::list_sources_for_series(&state.pool, id).await?;

    // Same-source smart merge (§10): a canonical series can carry several `series_sources`
    // rows for the *same* provider (a work split into two entries on that site, merged into
    // one series). Those are one work, not two adapter sources, so fold every provider's rows
    // into a single reader-visible "completing" source before building the DTOs.
    let groups = group_sources_by_provider(&sources);

    // Reader-facing count per merged source: distinct whole chapters (§ chapter grouping)
    // across *all* of the provider's entries — part releases and chapters two entries happen
    // to share never inflate the "Read on" card / hero stat.
    let mut chapter_counts = Vec::with_capacity(groups.len());
    for group in &groups {
        chapter_counts.push(
            tankovault_db::repo::catalog::count_full_chapters_across(&state.pool, &group.member_ids)
                .await?,
        );
    }
    // The primary source is the merged provider carrying the most chapters (ties → first).
    let primary_idx = chapter_counts
        .iter()
        .enumerate()
        .max_by_key(|(_, count)| **count)
        .map(|(i, _)| i);

    let mut source_dtos = Vec::with_capacity(groups.len());
    for (i, group) in groups.iter().enumerate() {
        let provider = tankovault_db::repo::providers::get(&state.pool, group.provider_id).await?;
        // The outbound link points at the richest entry's page on the provider.
        let url = resolve_link(&provider.base_url, &group.link_source_path)
            .map_err(|_| ApiError::Internal)?;
        source_dtos.push(SourceDto {
            id: group.link_id,
            provider_name: provider.name,
            provider_slug: provider.slug,
            url,
            chapter_count: chapter_counts[i],
            is_primary: Some(i) == primary_idx,
        });
    }

    let alt_titles = tankovault_db::repo::catalog::list_series_titles(&state.pool, id).await?;
    let tags = tankovault_db::repo::catalog::list_series_tags(&state.pool, id).await?;
    let authors = tankovault_db::repo::catalog::list_series_authors(&state.pool, id).await?;
    let anilist_id =
        tankovault_db::repo::sync::mapping_external_for_series(&state.pool, id, "anilist").await?;

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
        authors,
        anilist_id,
    }))
}

/// One provider's folded presence on a canonical series: all of its (possibly several)
/// `series_sources` rows collapsed into a single reader-visible "completing" source
/// (design §10 same-source smart merge). `member_ids` are every underlying source row whose
/// chapters together form the merged list; `link_id` / `link_source_path` identify the richest
/// member (most raw chapters) — used as the DTO's representative id and outbound provider link.
struct ProviderGroup {
    provider_id: ProviderId,
    link_id: SeriesSourceId,
    link_source_path: String,
    member_ids: Vec<SeriesSourceId>,
}

/// Fold a series' source rows so each provider appears exactly once. Within a canonical
/// series, every `series_sources` row sharing a provider is the same work split across
/// provider entries (the canonicalisation merge already decided so, design §10), so the reader
/// should see the provider once — with the union of its chapters — rather than one card per
/// split entry. First-seen provider order is preserved (matching `list_sources_for_series`,
/// which orders by id), and within a provider the richest entry becomes the outbound link.
fn group_sources_by_provider(sources: &[SeriesSource]) -> Vec<ProviderGroup> {
    // Parallel `best_counts` tracks each group's richest member's raw chapter_count so the
    // link target only advances to a strictly richer entry.
    let mut groups: Vec<ProviderGroup> = Vec::new();
    let mut best_counts: Vec<i32> = Vec::new();
    for src in sources {
        if let Some(pos) = groups.iter().position(|g| g.provider_id == src.provider_id) {
            groups[pos].member_ids.push(src.id);
            if src.chapter_count > best_counts[pos] {
                best_counts[pos] = src.chapter_count;
                groups[pos].link_id = src.id;
                groups[pos].link_source_path.clone_from(&src.source_path);
            }
        } else {
            groups.push(ProviderGroup {
                provider_id: src.provider_id,
                link_id: src.id,
                link_source_path: src.source_path.clone(),
                member_ids: vec![src.id],
            });
            best_counts.push(src.chapter_count);
        }
    }
    groups
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ChapterParams {
    /// Which source to read chapters from. Defaults to the first source of the series.
    pub source: Option<SeriesSourceId>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ChapterDto {
    pub number: f64,
    pub title: Option<String>,
    /// Resolved absolute URL to open the chapter page on the provider.
    pub url: String,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub published_at: Option<time::OffsetDateTime>,
    /// Whether the requesting user has read this chapter (number ≤ their progress).
    /// `None` for anonymous requests; `Some(bool)` when a valid `Bearer` token is present
    /// (frontend §9.2 auth-scoped read-state).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read: Option<bool>,
}

/// List a source's chapters
///
/// Chapter list, newest first. When a valid access token is supplied the per-chapter `read`
/// flag is populated from the user's progress (frontend §9.2 auth-scoped read-state);
/// anonymous callers get the same list without read-state.
#[utoipa::path(
    get,
    path = "/v1/series/{id}/chapters",
    tag = SERIES_TAG,
    params(("id" = SeriesId, Path, description = "Series id"), ChapterParams),
    security((), ("bearer_auth" = [])),
    responses(
        (status = 200, description = "Chapters, newest first", body = Vec<ChapterDto>),
        (status = 404, description = "Series (or its source) not found", body = crate::error::ProblemDetails),
    )
)]
pub async fn chapters(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<SeriesId>,
    Query(params): Query<ChapterParams>,
) -> ApiResult<Json<Vec<ChapterDto>>> {
    let sources = tankovault_db::repo::catalog::list_sources_for_series(&state.pool, id).await?;

    // Resolve the requested source (explicit `?source=` or the series' first source), then
    // expand it to every sibling entry of the *same* provider on this series: a provider split
    // across several `series_sources` rows is one merged "completing" source (§10 smart merge),
    // so its chapter list is the de-duplicated union of all those entries, not just one.
    let target_id = match params.source {
        Some(s) => s,
        None => sources.first().map(|s| s.id).ok_or(ApiError::NotFound)?,
    };
    let provider_id = sources
        .iter()
        .find(|s| s.id == target_id)
        .map(|s| s.provider_id)
        .ok_or(ApiError::NotFound)?;
    let member_ids: Vec<SeriesSourceId> = sources
        .iter()
        .filter(|s| s.provider_id == provider_id)
        .map(|s| s.id)
        .collect();

    // All members share the provider, so one base_url resolves every chapter's relative path.
    let (_, base_url) =
        tankovault_db::repo::catalog::source_provider_base_url(&state.pool, target_id).await?;
    let chapters =
        tankovault_db::repo::catalog::list_chapters_across(&state.pool, &member_ids).await?;

    // Read-state is opt-in: only when a valid token identifies the user. An authenticated
    // user with no progress row yet still gets `Some(false)` per chapter (they simply
    // haven't read anything), so the frontend shows the mark-read control; only anonymous
    // callers get `None`. This is independent of any external (AniList) link.
    let user = optional_user(&state, &headers);
    let progress = match user {
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
                read: user
                    .is_some()
                    .then(|| progress.is_some_and(|last| c.number <= last)),
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

/// List all tags
///
/// All genres/tags in the catalogue (public).
#[utoipa::path(
    get,
    path = "/v1/tags",
    tag = SERIES_TAG,
    responses((status = 200, description = "All known tags", body = Vec<tankovault_domain::Tag>))
)]
pub async fn tags(State(state): State<AppState>) -> ApiResult<Json<Vec<tankovault_domain::Tag>>> {
    Ok(Json(
        tankovault_db::repo::catalog::list_tags(&state.pool).await?,
    ))
}

/// List public providers
///
/// Provider list + per-provider series counts, for the Discover provider filter (frontend
/// §9.3). Operator-only fields (config/politeness/health) are not exposed; disabled providers
/// are hidden.
#[utoipa::path(
    get,
    path = "/v1/providers",
    tag = SERIES_TAG,
    responses(
        (status = 200, description = "Enabled providers", body = Vec<tankovault_db::repo::providers::PublicProvider>),
    )
)]
pub async fn providers(
    State(state): State<AppState>,
) -> ApiResult<Json<Vec<tankovault_db::repo::providers::PublicProvider>>> {
    Ok(Json(
        tankovault_db::repo::providers::list_public(&state.pool).await?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tankovault_domain::ProviderState;

    /// Build a minimal `SeriesSource` for a given provider, path, and raw chapter count.
    /// The other fields are irrelevant to same-provider folding.
    fn source(provider_id: ProviderId, source_path: &str, chapter_count: i32) -> SeriesSource {
        SeriesSource {
            id: SeriesSourceId::new(),
            series_id: SeriesId::new(),
            provider_id,
            source_path: source_path.to_owned(),
            provider_title: None,
            content_hash: None,
            chapter_count,
            last_scanned_at: None,
            state: ProviderState::default(),
        }
    }

    #[test]
    fn folds_same_provider_entries_into_one_completing_source() {
        // Two KunManga entries under one canonical series: the exact issue case.
        let kunmanga = ProviderId::new();
        let early = source(kunmanga, "/manga/work-part-1", 40);
        let later = source(kunmanga, "/manga/work-part-2", 120);

        let groups = group_sources_by_provider(&[early.clone(), later.clone()]);

        assert_eq!(groups.len(), 1, "one provider must yield one merged source");
        let group = &groups[0];
        assert_eq!(group.provider_id, kunmanga);
        assert_eq!(group.member_ids, vec![early.id, later.id]);
        // The richer entry (120 > 40) drives the outbound link/representative id.
        assert_eq!(group.link_id, later.id);
        assert_eq!(group.link_source_path, "/manga/work-part-2");
    }

    #[test]
    fn keeps_distinct_providers_separate_in_first_seen_order() {
        let a = ProviderId::new();
        let b = ProviderId::new();
        let sa = source(a, "/a", 10);
        let sb = source(b, "/b", 20);

        let groups = group_sources_by_provider(&[sa.clone(), sb.clone()]);

        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].provider_id, a);
        assert_eq!(groups[1].provider_id, b);
        assert_eq!(groups[0].member_ids, vec![sa.id]);
        assert_eq!(groups[1].member_ids, vec![sb.id]);
    }

    #[test]
    fn richest_link_holds_on_a_later_smaller_entry() {
        // A smaller entry seen after the richest one must not steal the link target.
        let provider = ProviderId::new();
        let big = source(provider, "/big", 200);
        let small = source(provider, "/small", 5);

        let groups = group_sources_by_provider(&[big.clone(), small.clone()]);

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].link_id, big.id);
        assert_eq!(groups[0].member_ids, vec![big.id, small.id]);
    }

    #[test]
    fn empty_input_yields_no_groups() {
        assert!(group_sources_by_provider(&[]).is_empty());
    }
}
