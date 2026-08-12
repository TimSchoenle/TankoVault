//! Public series browse/detail/chapters. Links are resolved to absolute URLs here via
//! `domain::resolve_link`; the database stays relative (design §11).

use crate::content_gate::AdultVisibility;
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
    /// `tracked` narrows the list to series the caller already has on their watchlist,
    /// `untracked` to the rest. Requires an authenticated caller — whose watchlist it is comes
    /// from the token, never from a parameter.
    #[serde(default)]
    pub tracking: Option<String>,
    /// `relevance | updated | title | chapters | sources | year | rating`. Defaults to
    /// `relevance` when `query` is supplied and `updated` when it is not; `relevance` without a
    /// `query` has nothing to rank and falls back to `updated`.
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

/// The `tracking` parameter: which side of the caller's own watchlist the list is narrowed to.
///
/// An enum with a strict [`FromStr`](std::str::FromStr) rather than a bare string, so a typo is a
/// `400` naming the parameter instead of a silently unfiltered page — the same reasoning as
/// `sort`, and the more important one here: "hide what I already read" reads as working when it
/// quietly does nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackingFilter {
    /// Only series on the caller's watchlist.
    Tracked,
    /// Only series that are not.
    Untracked,
}

impl std::str::FromStr for TrackingFilter {
    type Err = ();

    fn from_str(token: &str) -> Result<Self, Self::Err> {
        match token {
            "tracked" => Ok(Self::Tracked),
            "untracked" => Ok(Self::Untracked),
            _ => Err(()),
        }
    }
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

/// One card in a catalogue grid.
///
/// Grown rather than replaced: a deployed SPA outlives a server change, so every field an older
/// client reads is still here and the additions are ignored by it. `source_count` is kept for
/// that reason alone — the surfaces that used to print it now print [`Self::chapter_count`],
/// which is what a reader was actually trying to learn from it.
#[derive(Debug, Serialize, ToSchema)]
pub struct SeriesSummary {
    pub id: SeriesId,
    pub title: String,
    pub cover_url: Option<String>,
    pub content_type: ContentType,
    pub status: SeriesStatus,
    /// Distinct providers carrying this series. Retained for compatibility; not a figure any
    /// reader-facing surface shows.
    pub source_count: i64,
    /// Distinct **whole** chapters across every source, so a title carried by four providers is
    /// not counted four times and a part release does not count as its own chapter. This is the
    /// same figure the series screen prints, so a card and the page it opens agree.
    pub chapter_count: i64,
    /// The highest chapter number any source carries, when there is one.
    pub latest_chapter: Option<f64>,
    pub release_year: Option<i32>,
    /// The opening of the description, trimmed to a card's worth. `None` when the series has
    /// none, which a card renders as nothing rather than as empty space.
    pub blurb: Option<String>,
    /// Tag names, alphabetically. Capped server-side: a card has room for two or three, and
    /// shipping forty for a client to slice is payload nobody renders.
    pub tags: Vec<String>,
    /// Whether this series is adult-classified.
    ///
    /// Only ever `true` for a caller who opted in — a card that reaches a client at all has
    /// already passed the gate. This labels what is on screen; it is not what hides anything.
    pub is_adult: bool,
}

/// Tags a card carries at most. Alphabetical, so the choice is stable between requests rather
/// than varying with row order.
const CARD_TAGS: usize = 3;

/// Characters a card's blurb may carry before it is cut at the preceding word boundary.
///
/// Sized for the three lines a cover card has room for. Trimming here rather than in the client
/// keeps a full description — which routinely runs to several kilobytes — out of a grid response
/// that carries sixty of them.
const BLURB_CHARS: usize = 220;

/// A series description as a card shows it: whitespace collapsed, cut at a word boundary, with an
/// ellipsis when anything was dropped.
///
/// `None` for an absent or blank description, so a card branches on presence rather than
/// rendering an empty line.
#[must_use]
pub fn blurb(description: Option<&str>) -> Option<String> {
    let collapsed = description?
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if collapsed.is_empty() {
        return None;
    }
    if collapsed.chars().count() <= BLURB_CHARS {
        return Some(collapsed);
    }
    // Cut on the last space inside the budget so the trailing word is whole; a description with
    // no space in that span (CJK, which is most of this catalogue) falls back to the hard cut.
    let head: String = collapsed.chars().take(BLURB_CHARS).collect();
    let cut = head.rfind(' ').unwrap_or(head.len());
    let kept = head[..cut].trim_end_matches([',', '.', ';', ':', ' ']);
    Some(format!("{kept}…"))
}

impl SeriesSummary {
    /// Build the summaries for one page, in the order given, with the two batched reads the
    /// card needs.
    ///
    /// Batched rather than folded into the listing query: the browse statements are the ones the
    /// plan audit budgets, and a reach into `chapters` per candidate row is charged against every
    /// row of `series` under a generic plan. Keyed on the ids about to be rendered, these two
    /// touch only those.
    ///
    /// # Errors
    /// Propagates the two reads. Neither is defaulted away — a card silently claiming zero
    /// chapters is worse than the page failing and being retried.
    async fn page(
        state: &AppState,
        items: Vec<tankovault_db::repo::catalog::SeriesListItem>,
    ) -> ApiResult<Vec<Self>> {
        let ids: Vec<SeriesId> = items.iter().map(|it| it.series.id).collect();
        let (chapters, tags, adult) = tokio::try_join!(
            tankovault_db::repo::catalog::chapter_stats_for_series(&state.pool, &ids),
            tankovault_db::repo::catalog::tags_for_series(&state.pool, &ids),
            tankovault_db::repo::catalog::adult_gated_many(&state.pool, &ids),
        )?;

        Ok(items
            .into_iter()
            .map(|it| {
                let id = it.series.id;
                let counts = chapters.get(&id);
                Self {
                    id,
                    title: it.series.canonical_title,
                    cover_url: it.series.cover_url,
                    content_type: it.series.content_type,
                    status: it.series.status,
                    source_count: it.source_count,
                    chapter_count: counts.map_or(0, |c| c.chapter_count),
                    latest_chapter: counts.and_then(|c| c.latest_number),
                    release_year: it.series.release_year,
                    blurb: blurb(it.series.description.as_deref()),
                    tags: tags
                        .get(&id)
                        .map(|names| names.iter().take(CARD_TAGS).cloned().collect())
                        .unwrap_or_default(),
                    is_adult: adult.contains(&id),
                }
            })
            .collect())
    }
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
        (status = 401, description = "`tracking` was supplied without an authenticated caller", body = crate::error::ProblemDetails),
    )
)]
pub async fn list(
    State(state): State<AppState>,
    adult: AdultVisibility,
    headers: HeaderMap,
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
    // A *searched* list defaults to relevance, an unsearched one to recency. Without this the
    // search screen inherited `updated`, so an exact title match ranked wherever its last scan
    // happened to put it — which is how a 100%-matching title came back below forty unrelated
    // series. An explicit `sort` still wins: the Discover grid's control is the same parameter.
    let searching = params
        .query
        .as_deref()
        .is_some_and(|q| !q.trim().is_empty());
    let sort = parse_param(params.sort.as_deref(), "sort")?.unwrap_or({
        if searching {
            tankovault_db::repo::catalog::SeriesSort::Relevance
        } else {
            tankovault_db::repo::catalog::SeriesSort::default()
        }
    });
    let content_type = parse_param(params.content_type.as_deref(), "content_type")?;
    let status = parse_param(params.status.as_deref(), "status")?;

    // Whose watchlist comes from the token, never from the query string: a browse parameter that
    // could name an account would read one reader's shelf back to another.
    //
    // Refused rather than ignored for an anonymous caller. "Hide what I already read" that
    // silently returns everything is the failure mode this whole parameter exists to avoid.
    let tracking: Option<TrackingFilter> = parse_param(params.tracking.as_deref(), "tracking")?;
    let reader = match tracking {
        None => None,
        Some(_) => Some(optional_user(&state, &headers).ok_or(ApiError::Unauthorized)?),
    };

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
        include_adult: adult.include_adult(),
        tracked_by: reader.map(UserId::as_uuid),
        tracked: tracking.map(|mode| mode == TrackingFilter::Tracked),
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

    let items = SeriesSummary::page(&state, out.items).await?;
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
    /// Whether this series is adult-classified. See [`SeriesSummary::is_adult`] — reaching this
    /// response at all means the caller is entitled to it.
    pub is_adult: bool,
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
    adult: AdultVisibility,
    Path(id): Path<SeriesId>,
) -> ApiResult<Json<SeriesDetail>> {
    use tankovault_db::repo::{catalog, providers, sync};

    // Two grouped reads replace what used to be an N+1 loop over providers; the four
    // independent tail reads below touch different tables and can overlap.
    let series = catalog::get_series_visible(&state.pool, id, adult.include_adult()).await?;
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
    let only = [id];
    let (alt_titles, tags, authors, anilist_id, adult) = tokio::try_join!(
        catalog::list_series_titles(&state.pool, id),
        catalog::list_series_tags(&state.pool, id),
        catalog::list_series_authors(&state.pool, id),
        sync::mapping_external_for_series(&state.pool, id, "anilist"),
        catalog::adult_gated_many(&state.pool, &only),
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
        is_adult: adult.contains(&id),
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

    // The same identity decides which chapters exist at all: a provider's paid early-access rows
    // are listed only to a reader who has opted that provider in. Everything downstream — the
    // counts on this screen, the "next up" marker, the merged open control — is derived from
    // this list, so filtering here is what keeps all of them from offering a paywall.
    let chapters =
        tankovault_db::repo::catalog::list_chapters_across(&state.pool, &member_ids, user).await?;

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

/// One entry of the tag facet: a tag plus how much of the catalogue carries it.
#[derive(Debug, Serialize, ToSchema)]
pub struct TagFacet {
    pub id: tankovault_domain::TagId,
    pub slug: String,
    pub name: String,
    /// Series carrying this tag. What lets the filter panel order by usage and say how much a
    /// chip would narrow the grid before it is clicked.
    pub series_count: i64,
}

/// List all tags
///
/// Every genre/tag in the catalogue with the number of series carrying it, commonest first
/// (public).
///
/// Ordered by usage rather than alphabetically because the facet panel that consumes this can
/// only show so many chips at once: an alphabetical list truncated to fit cuts off at whatever
/// letter the cap lands on, hiding the genres most of the catalogue actually uses. The body is a
/// superset of the previous `Tag[]`, so an older client reading only `id`/`slug`/`name` is
/// unaffected — it just sees them in a different order.
///
/// Adult-classifying genres are withheld from a caller the gate closes on. They are the terms
/// that put a series behind [`crate::content_gate`] in the first place, so offering them as
/// filter chips advertises a slice of the catalogue the same request cannot return a single row
/// of — and names it in the reader's own filter panel, which is the part the gate exists to
/// avoid.
#[utoipa::path(
    get,
    path = "/v1/tags",
    tag = SERIES_TAG,
    responses((status = 200, description = "All known tags, commonest first", body = Vec<TagFacet>))
)]
pub async fn tags(
    State(state): State<AppState>,
    adult: AdultVisibility,
) -> ApiResult<Json<Vec<TagFacet>>> {
    let rows = tankovault_db::repo::catalog::list_tag_facets(&state.pool).await?;
    let gated = !adult.include_adult();
    Ok(Json(
        rows.into_iter()
            .filter(|row| !(gated && state.adult_tags.classifies(&row.tag.slug)))
            .map(|row| TagFacet {
                id: row.tag.id,
                slug: row.tag.slug,
                name: row.tag.name,
                series_count: row.series_count,
            })
            .collect(),
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

/// One similar series, with what it has in common.
#[derive(Debug, Serialize, ToSchema)]
pub struct SimilarSeries {
    #[serde(flatten)]
    pub series: SeriesSummary,
    /// Cosine similarity in `[0, 1]`, as the index ranked it.
    pub score: f32,
    /// The features this shares with the seed, most explanatory first — the tag names a reader
    /// would recognise, not feature ids.
    pub shared: Vec<String>,
}

/// How many neighbours a single request may ask for.
const MAX_SIMILAR: i64 = 50;

/// Over-fetch factor for the ANN scan.
///
/// Filters (recommendable, adult, the seed itself) are applied *after* the index has ranked, so
/// the scan has to return more than the caller wants or a deployment with many unrecommendable
/// series quietly returns short pages. Four is generous for the default filter selectivity and
/// still bounded.
const SIMILAR_OVERFETCH: i64 = 4;

/// Get similar series
///
/// Content-similar series, ranked by an approximate nearest-neighbour search over the
/// recommendation model's embedding space, with the features each match shares with the seed.
///
/// Falls back to the catalogue's popularity prior when the seed has no embedding yet — a series
/// added since the last model build, or a deployment that has never run one. An empty array
/// means the model has never been built at all.
#[utoipa::path(
    get,
    path = "/v1/series/{id}/similar",
    tag = SERIES_TAG,
    params(
        ("id" = SeriesId, Path, description = "Series id"),
        ("limit" = Option<i64>, Query, description = "How many to return (default 12, max 50)"),
    ),
    responses(
        (status = 200, description = "Similar series, closest first", body = Vec<SimilarSeries>),
        (status = 404, description = "Series not found", body = crate::error::ProblemDetails),
    )
)]
pub async fn similar(
    State(state): State<AppState>,
    adult: AdultVisibility,
    Path(id): Path<SeriesId>,
    Query(params): Query<SimilarParams>,
) -> ApiResult<Json<Vec<SimilarSeries>>> {
    use tankovault_db::repo::{matching, recsys};

    let limit = params.limit.unwrap_or(12).clamp(1, MAX_SIMILAR);

    // A merged id must answer, not 404: automatic merges run continuously, so any id a client
    // holds may have been absorbed since it was handed out.
    let seed = match matching::resolve_merged_series(&state.pool, id).await? {
        Some(survivor) => survivor,
        None => id,
    };
    // Confirms the series exists at all, and turns an unknown id into a 404 rather than an
    // empty list — the two mean different things to a caller.
    tankovault_db::repo::catalog::get_series_visible(&state.pool, seed, adult.include_adult())
        .await?;

    let Some(embedding) = recsys::embedding_of(&state.pool, seed).await? else {
        return Ok(Json(fallback_to_prior(&state, seed, adult, limit).await?));
    };

    let neighbours = recsys::nearest_neighbours(
        &state.pool,
        &embedding,
        seed,
        adult.include_adult(),
        limit,
        limit * SIMILAR_OVERFETCH,
    )
    .await?;
    if neighbours.is_empty() {
        return Ok(Json(fallback_to_prior(&state, seed, adult, limit).await?));
    }

    let ids: Vec<SeriesId> = neighbours.iter().map(|n| n.series_id).collect();
    let summaries = recsys::summaries_in_order(&state.pool, &ids).await?;

    // The embedding says *that* these are close; only the sparse vectors say why. One bounded
    // join over the seed plus its neighbours.
    let mut vector_ids = vec![seed];
    vector_ids.extend(ids.iter().copied());
    let (vectors, features) = recsys::weighted_vectors(&state.pool, &vector_ids).await?;
    let vector_of: HashMap<SeriesId, Vec<(i32, f32)>> = vectors.into_iter().collect();
    let name_of: HashMap<i32, String> = features.into_iter().map(|f| (f.id, f.value)).collect();
    let seed_vector = vector_of.get(&seed).cloned().unwrap_or_default();

    let score_of: HashMap<SeriesId, f32> =
        neighbours.iter().map(|n| (n.series_id, n.score)).collect();

    let shared_of: HashMap<SeriesId, Vec<String>> = summaries
        .iter()
        .map(|item| {
            let series_id = item.series.id;
            let shared: Vec<String> = vector_of
                .get(&series_id)
                .map(|other| tankovault_recsys::shared_features(&seed_vector, other, 3))
                .unwrap_or_default()
                .into_iter()
                .filter_map(|feature_id| name_of.get(&feature_id).cloned())
                .collect();
            (series_id, shared)
        })
        .collect();

    let out = SeriesSummary::page(&state, summaries)
        .await?
        .into_iter()
        .map(|series| SimilarSeries {
            score: score_of.get(&series.id).copied().unwrap_or_default(),
            shared: shared_of.get(&series.id).cloned().unwrap_or_default(),
            series,
        })
        .collect();
    Ok(Json(out))
}

/// Query parameters for [`similar`].
#[derive(Debug, Deserialize, IntoParams)]
pub struct SimilarParams {
    pub limit: Option<i64>,
}

/// The shelf when the model cannot answer: broadly appealing series, no explanation offered.
///
/// Returns an empty list rather than an error when the model has never been built. A caller
/// cannot act on the difference, and a 500 for "the nightly job has not run yet" would take the
/// series page down with it.
async fn fallback_to_prior(
    state: &AppState,
    seed: SeriesId,
    adult: AdultVisibility,
    limit: i64,
) -> ApiResult<Vec<SimilarSeries>> {
    use tankovault_db::repo::recsys;

    let ids: Vec<SeriesId> = recsys::top_by_prior(&state.pool, adult.include_adult(), limit + 1)
        .await?
        .into_iter()
        .filter(|id| *id != seed)
        .take(usize::try_from(limit).unwrap_or(12))
        .collect();
    let summaries = recsys::summaries_in_order(&state.pool, &ids).await?;
    Ok(SeriesSummary::page(state, summaries)
        .await?
        .into_iter()
        .map(|series| SimilarSeries {
            score: 0.0,
            shared: Vec::new(),
            series,
        })
        .collect())
}
