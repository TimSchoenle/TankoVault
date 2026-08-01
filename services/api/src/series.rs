//! Public series browse/detail/chapters. Links are resolved to absolute URLs here via
//! `domain::resolve_link`; the database stays relative (design §11).

use crate::error::{ApiError, ApiResult};
use crate::state::AppState;
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue};
use axum_extra::extract::Query as MultiQuery;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tankovault_db::repo::catalog::SeriesFilter;
use tankovault_domain::{
    ContentType, Feature, ProviderId, SeriesId, SeriesSource, SeriesSourceId, SeriesStatus, UserId,
    resolve_link,
};
use utoipa::{IntoParams, ToSchema};

use crate::openapi::SERIES_TAG;
use crate::views::IntoView;

/// Highest accepted page index for the browse listing.
///
/// At the 100-item maximum page size this is 10 million rows deep — far past any real
/// catalogue, and far short of the `i64` overflow the unbounded value allowed.
const MAX_PAGE: i64 = 100_000;

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
    /// `updated | title | chapters | sources | year | rating` (default `updated`).
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

/// Parse an optional query-string token, refusing an unrecognised one.
///
/// An empty value means "not supplied" — the frontend's select controls submit `""` for their
/// "any" option, and treating that as a parse failure would 400 the default page.
fn parse_param<T: std::str::FromStr>(raw: Option<&str>, name: &str) -> ApiResult<Option<T>> {
    match raw.map(str::trim).filter(|v| !v.is_empty()) {
        None => Ok(None),
        Some(value) => value
            .parse()
            .map(Some)
            .map_err(|_| ApiError::BadRequest(format!("unknown {name}: {value:?}"))),
    }
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
    // Search is a *parameter* of the browse route, not a route of its own, so the feature
    // table can't express it. Refused rather than silently ignored — a quietly unfiltered
    // catalogue is worse than a clear no.
    if params
        .query
        .as_deref()
        .is_some_and(|q| !q.trim().is_empty())
        && !state.features.is_enabled(Feature::CatalogueSearch)
    {
        return Err(ApiError::FeatureDisabled(Feature::CatalogueSearch));
    }

    let limit = params.limit.clamp(1, 100);
    // Unbounded `page * limit` overflows `i64` in release (`overflow-checks = false`),
    // wrapping to a negative offset; debug/CI builds panic instead, aborting the process.
    //
    // Clamp is the real bound; `saturating_mul` keeps the arithmetic total regardless.
    let page = params
        .page
        .or(params.cursor)
        .unwrap_or(0)
        .clamp(0, MAX_PAGE);
    // Parsed at the edge: an unrecognised `sort`/`content_type`/`status` must fail loudly,
    // not silently answer `200` with a wrong page. Native Postgres types also let the
    // filters use an index; casting the column to text would lose it.
    let sort = parse_param(params.sort.as_deref(), "sort")?.unwrap_or_default();
    let content_type = parse_param(params.content_type.as_deref(), "content_type")?;
    let status = parse_param(params.status.as_deref(), "status")?;

    let filter = SeriesFilter {
        query: params.query,
        content_type,
        status,
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
        sort,
        limit,
        offset: page.saturating_mul(limit),
    };
    let out = tankovault_db::repo::catalog::list_series_filtered(&state.pool, &filter).await?;

    let mut headers = HeaderMap::new();
    if let Ok(v) = HeaderValue::from_str(&out.total.to_string()) {
        headers.insert("X-Total-Count", v);
    }
    let returned = i64::try_from(out.items.len()).unwrap_or(0);
    if filter.offset + returned < out.total
        && let Ok(v) = HeaderValue::from_str(&(page + 1).to_string())
    {
        headers.insert("X-Next-Cursor", v);
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
    use tankovault_db::repo::{catalog, providers, sync};

    // Two grouped reads replace what used to be an N+1 loop over providers; the four
    // independent tail reads below touch different tables and can overlap.
    let series = catalog::get_series(&state.pool, id).await?;
    let sources = catalog::list_sources_for_series(&state.pool, id).await?;

    // Same-source smart merge (§10): several `series_sources` rows can share a provider (one
    // work split across site entries). Fold them into one reader-visible "completing" source.
    let groups = group_sources_by_provider(&sources);

    // Distinct whole chapters (§ chapter grouping) across all of a provider's entries, so
    // part releases and shared chapters never inflate the "Read on" card. Grouping in SQL
    // mirrors `group_sources_by_provider`'s fold; a provider with no chapters counts zero.
    let counts_by_provider: HashMap<ProviderId, i32> =
        catalog::count_full_chapters_by_provider(&state.pool, id)
            .await?
            .into_iter()
            .collect();
    let chapter_counts: Vec<i32> = groups
        .iter()
        .map(|g| counts_by_provider.get(&g.provider_id).copied().unwrap_or(0))
        .collect();

    // The primary source is the merged provider carrying the most chapters (ties → first).
    let primary_idx = chapter_counts
        .iter()
        .enumerate()
        .max_by_key(|(_, count)| **count)
        .map(|(i, _)| i);

    let provider_ids: Vec<ProviderId> = groups.iter().map(|g| g.provider_id).collect();
    let providers: HashMap<ProviderId, tankovault_domain::Provider> =
        providers::get_many(&state.pool, &provider_ids)
            .await?
            .into_iter()
            .map(|p| (p.id, p))
            .collect();

    let mut source_dtos = Vec::with_capacity(groups.len());
    for (i, group) in groups.iter().enumerate() {
        // Unreachable today (the foreign key guarantees it); treated as "not found" rather
        // than unwrapped so it stays that way if the constraint ever changes.
        let provider = providers
            .get(&group.provider_id)
            .ok_or(ApiError::NotFound)?;
        // The outbound link points at the richest entry's page on the provider.
        let url = resolve_link(&provider.base_url, &group.link_source_path)
            .map_err(|_| ApiError::Internal)?;
        source_dtos.push(SourceDto {
            id: group.link_id,
            provider_name: provider.name.clone(),
            provider_slug: provider.slug.clone(),
            url,
            chapter_count: chapter_counts[i],
            is_primary: Some(i) == primary_idx,
        });
    }

    // Four different tables, no shared state, nothing downstream of one another.
    let (alt_titles, tags, authors, anilist_id) = tokio::try_join!(
        catalog::list_series_titles(&state.pool, id),
        catalog::list_series_tags(&state.pool, id),
        catalog::list_series_authors(&state.pool, id),
        sync::mapping_external_for_series(&state.pool, id, "anilist"),
    )?;

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

/// One provider's folded presence on a canonical series (design §10): several `series_sources`
/// rows collapsed into one reader-visible "completing" source. `link_id`/`link_source_path`
/// identify the richest member — the DTO's representative id and outbound link.
struct ProviderGroup {
    provider_id: ProviderId,
    link_id: SeriesSourceId,
    link_source_path: String,
    member_ids: Vec<SeriesSourceId>,
}

/// Fold a series' source rows so each provider appears exactly once (design §10). First-seen
/// provider order is preserved (matching `list_sources_for_series`, which orders by id); the
/// richest entry within a provider becomes the outbound link.
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

    // Resolve the requested source, then expand to every sibling entry of the same provider:
    // a provider split across rows is one merged "completing" source, so the list is their union.
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

    // Read-state is opt-in: only a valid token gets it; no progress row yet still yields
    // `Some(false)`, not `None` (which means anonymous).
    //
    // Both frontiers are needed — a part read ahead of the whole frontier lives only in
    // `last_read_part_number`, so checking just the whole frontier reports it unread forever.
    let user = optional_user(&state, &headers);
    let progress = match user {
        Some(user_id) => {
            tankovault_db::repo::tracking::progress_get_full(&state.pool, user_id, id).await?
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
                    .then(|| progress.is_some_and(|p| p.covers(c.number))),
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
        (status = 200, description = "Enabled providers", body = Vec<tankovault_contracts::catalogue::PublicProviderView>),
    )
)]
pub async fn providers(
    State(state): State<AppState>,
) -> ApiResult<Json<Vec<tankovault_contracts::catalogue::PublicProviderView>>> {
    let rows = tankovault_db::repo::providers::list_public(&state.pool).await?;
    Ok(Json(rows.into_view()))
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
