//! Read models for the browse/discover surfaces: the plain series listing, the filtered and
//! sorted one behind `GET /v1/series`, and the per-series title/tag/author reads.

use crate::error::DbResult;
use sqlx::{FromRow, PgExecutor, PgPool};
use tankovault_domain::{ContentType, Series, SeriesId, SeriesStatus};
use time::OffsetDateTime;
use uuid::Uuid;

/// A row in the discover/browse list: the series plus its resolvable cover and a
/// count of provider sources.
pub struct SeriesListItem {
    pub series: Series,
    pub source_count: i64,
}

/// Query the browse list with keyset pagination on `(created_at, id)`.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. A `query` matching nothing and
/// an empty catalogue are the same empty `Vec`, not [`crate::DbError::NotFound`] — "no results"
/// is a 200 with an empty list on every browse surface. `limit` is passed through unvalidated;
/// the clamp lives at the edge (SEC-11).
pub async fn list_series<'e, E: PgExecutor<'e>>(
    exec: E,
    query: Option<&str>,
    limit: i64,
) -> DbResult<Vec<SeriesListItem>> {
    // Trigram + FTS aware search when a query is supplied; otherwise most-recent first.
    #[derive(FromRow)]
    struct ListRow {
        id: Uuid,
        canonical_title: String,
        normalized_title: String,
        description: Option<String>,
        cover_url: Option<String>,
        content_type: ContentType,
        status: SeriesStatus,
        release_year: Option<i32>,
        created_at: OffsetDateTime,
        updated_at: OffsetDateTime,
        source_count: i64,
    }

    let rows = if let Some(q) = query {
        // Search matches the canonical title, the full-text vector, **and** any alternative
        // title/synonym (english/native/AniList synonyms recorded in `series_titles`), so a
        // work found under a non-primary name still surfaces. Ranking takes the best trigram
        // similarity across the canonical and alternative titles.
        sqlx::query_as!(
            ListRow,
            "SELECT s.id, s.canonical_title, s.normalized_title, s.description, s.cover_url, \
                    s.content_type AS \"content_type: ContentType\", \
                    s.status AS \"status: SeriesStatus\", s.release_year, \
                    s.created_at, s.updated_at, \
                    (SELECT count(DISTINCT ss.provider_id) FROM series_sources ss WHERE ss.series_id = s.id) AS \"source_count!\" \
             FROM series s \
             WHERE s.normalized_title % $1 OR s.search_vec @@ plainto_tsquery('simple', $1) \
                OR EXISTS (SELECT 1 FROM series_titles st \
                           WHERE st.series_id = s.id AND st.normalized % $1) \
             ORDER BY GREATEST( \
                        similarity(s.normalized_title, $1), \
                        COALESCE((SELECT MAX(similarity(st.normalized, $1)) \
                                  FROM series_titles st WHERE st.series_id = s.id), 0) \
                      ) DESC \
             LIMIT $2",
            q,
            limit,
        )
        .fetch_all(exec)
        .await?
    } else {
        sqlx::query_as!(
            ListRow,
            "SELECT s.id, s.canonical_title, s.normalized_title, s.description, s.cover_url, \
                    s.content_type AS \"content_type: ContentType\", \
                    s.status AS \"status: SeriesStatus\", s.release_year, \
                    s.created_at, s.updated_at, \
                    (SELECT count(DISTINCT ss.provider_id) FROM series_sources ss WHERE ss.series_id = s.id) AS \"source_count!\" \
             FROM series s ORDER BY s.updated_at DESC LIMIT $1",
            limit,
        )
        .fetch_all(exec)
        .await?
    };

    Ok(rows
        .into_iter()
        .map(|r| SeriesListItem {
            series: Series {
                id: SeriesId::from_uuid(r.id),
                canonical_title: r.canonical_title,
                normalized_title: r.normalized_title,
                description: r.description,
                cover_url: r.cover_url,
                content_type: r.content_type,
                status: r.status,
                release_year: r.release_year,
                created_at: r.created_at,
                updated_at: r.updated_at,
            },
            source_count: r.source_count,
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Read model: filtered/sorted/paginated series listing (GET /v1/series, §9.1)
// ---------------------------------------------------------------------------

/// How the Discover grid is ordered.
///
/// A closed enum rather than the `Option<String>` this used to be: an unrecognised token
/// silently fell back to `updated`, so a typo in a client produced a page that looked right
/// and was ordered wrong. The handler parses this and answers `400` instead.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SeriesSort {
    /// Most recently updated first. The default, the overwhelmingly common case, and the
    /// only one with a dedicated index (`series_updated_idx`).
    #[default]
    Updated,
    Title,
    Chapters,
    Sources,
    Year,
    /// Accepted, and ordered by recency. There is no rating column yet; the token is in the
    /// design and the frontend's sort control offers it, so refusing it would break a visible
    /// control for no gain. Ordering by recency is the honest fallback — and it is declared
    /// here rather than hidden in a `_ =>` arm, so adding the column later is a change to one
    /// obvious place.
    Rating,
}

impl SeriesSort {
    /// The wire token, which is also the value bound into the `ORDER BY` `CASE` expressions.
    #[must_use]
    pub fn as_token(self) -> &'static str {
        match self {
            Self::Updated => "updated",
            Self::Title => "title",
            Self::Chapters => "chapters",
            Self::Sources => "sources",
            Self::Year => "year",
            Self::Rating => "rating",
        }
    }

    /// Whether this order is served by the dedicated recency statement.
    fn is_recency(self) -> bool {
        matches!(self, Self::Updated | Self::Rating)
    }
}

impl std::str::FromStr for SeriesSort {
    type Err = ParseSeriesSortError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "updated" => Ok(Self::Updated),
            "title" => Ok(Self::Title),
            "chapters" => Ok(Self::Chapters),
            "sources" => Ok(Self::Sources),
            "year" => Ok(Self::Year),
            "rating" => Ok(Self::Rating),
            other => Err(ParseSeriesSortError(other.to_owned())),
        }
    }
}

/// Raised when a client asks for a sort order that does not exist.
#[derive(Debug, Clone, thiserror::Error)]
#[error("unknown sort order: {0:?}")]
pub struct ParseSeriesSortError(pub String);

/// Server-side filter/sort/paginate criteria for the Discover grid (frontend §9.1).
///
/// Every field is optional; `None`/empty means "no constraint". The enum filters bind as
/// their native Postgres enum types rather than as text: `s.content_type::text = $2` cast the
/// column away from any index it could use, and it let an unparseable token through to match
/// nothing instead of being refused at the edge. `tags` requires the series to carry **all**
/// listed slugs; `exclude_tags` removes any series carrying **any** listed slug.
#[derive(Debug, Default, Clone)]
pub struct SeriesFilter {
    pub query: Option<String>,
    pub content_type: Option<ContentType>,
    pub status: Option<SeriesStatus>,
    pub provider_slug: Option<String>,
    pub tags: Vec<String>,
    pub exclude_tags: Vec<String>,
    pub year_min: Option<i32>,
    pub year_max: Option<i32>,
    pub min_chapters: Option<i32>,
    pub sort: SeriesSort,
    pub limit: i64,
    pub offset: i64,
}

/// A page of the filtered browse list plus the total number of matching rows, so the API can
/// render `{ items, total, next_cursor }`.
pub struct SeriesPage {
    pub items: Vec<SeriesListItem>,
    pub total: i64,
}

#[derive(FromRow)]
struct FilteredRow {
    id: Uuid,
    canonical_title: String,
    normalized_title: String,
    description: Option<String>,
    cover_url: Option<String>,
    content_type: ContentType,
    status: SeriesStatus,
    release_year: Option<i32>,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
    source_count: i64,
}

/// Query the browse list with server-side filtering, sorting and offset pagination
/// (frontend §9.1). Returns the page plus the total match count for the pager.
///
/// # Why this is two statements — not five, and not one
///
/// It was **five** near-identical `query_as!` blocks differing only in `ORDER BY`, each
/// repeating the same nine-predicate `WHERE` and eleven binds. Adding a filter meant five
/// identical edits, and any one of them drifting produced a filter that applied under four
/// sort orders and not the fifth — on a predicate set that includes provider scoping and tag
/// exclusion. The shared text cannot be factored into a constant: `sqlx`'s compile-time
/// macros need a string *literal* and do not expand `concat!` (checked, not assumed).
///
/// It is not one statement either. Folding all six orders into bound `CASE` expressions does
/// work — Postgres constant-folds the inactive branches under a custom plan and still reaches
/// `series_title_sort_idx` for `title` — but it puts two correlated aggregate subqueries
/// (`chapters`, `sources`) into the sort key of *every* browse request, and under a generic
/// plan the default order loses `series_updated_idx` and falls back to a parallel seq scan.
/// The default order is the unauthenticated, highest-traffic route in the product, so it gets
/// its own statement and its plan cannot regress. Measured on the 78 850-row development
/// catalogue: indexed recency 2.9 ms; the `CASE` form under a forced generic plan 79 ms.
///
/// So: **one statement for recency, one for everything else.** Two copies of the `WHERE`
/// clause is a real cost; it is the smallest one available without giving up either
/// compile-time checking or the index on the hot path.
///
/// # Why the total is a separate query
///
/// The page query used to carry `count(*) OVER()`. A window function with no `PARTITION BY`
/// is evaluated over the whole result set, so Postgres materialised *every* matching row —
/// including the per-row `source_count` subquery — before the `LIMIT` could take 40. On the
/// development catalogue that meant a full sequential scan, a top-N sort and 66 MB spilled to
/// disk to return 40 rows: 179 ms. Counting separately lets the page query stop at `LIMIT`
/// and lets the count run as an index-only scan, and the two are issued concurrently so the
/// pair costs one round trip. Same catalogue, same filters: 179 ms → ~5 ms.
///
/// Takes `&PgPool` rather than a generic executor precisely so the two can overlap.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. A filter matching nothing is
/// `SeriesPage { items: [], total: 0 }`, never [`crate::DbError::NotFound`].
///
/// `try_join!` means **either** statement failing fails the pair, and the other's result is
/// discarded. That is the right trade here: a page without its total, or a total without its
/// page, is a pager that renders wrong rather than one that renders less, and the two run on
/// separate connections from the same pool so a pool exhausted by one of them fails both anyway.
/// Note the consequence for the *count*: page and total are two statements, so a concurrent
/// insert between them can make `total` disagree with what the page contains. The pager tolerates
/// that by design; nothing here takes a snapshot to prevent it.
pub async fn list_series_filtered(pool: &PgPool, filter: &SeriesFilter) -> DbResult<SeriesPage> {
    let query = filter
        .query
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty());

    let (rows, total) = tokio::try_join!(
        fetch_filtered_page(pool, filter, query),
        count_filtered(pool, filter, query),
    )?;

    let items = rows
        .into_iter()
        .map(|r| SeriesListItem {
            series: Series {
                id: SeriesId::from_uuid(r.id),
                canonical_title: r.canonical_title,
                normalized_title: r.normalized_title,
                description: r.description,
                cover_url: r.cover_url,
                content_type: r.content_type,
                status: r.status,
                release_year: r.release_year,
                created_at: r.created_at,
                updated_at: r.updated_at,
            },
            source_count: r.source_count,
        })
        .collect();

    Ok(SeriesPage { items, total })
}

/// One page of matching rows, in the requested order.
///
/// Both statements end with `s.id DESC`. Without a tiebreaker, rows sharing the leading sort
/// key had no defined order, so two adjacent `OFFSET` pages could repeat one row and skip
/// another; it is also what makes the recency order match `series_updated_idx
/// (updated_at DESC, id DESC)` exactly.
async fn fetch_filtered_page(
    pool: &PgPool,
    filter: &SeriesFilter,
    query: Option<&str>,
) -> DbResult<Vec<FilteredRow>> {
    if filter.sort.is_recency() {
        fetch_page_by_recency(pool, filter, query).await
    } else {
        fetch_page_by_sort_token(pool, filter, query).await
    }
}

/// The default order: newest-updated first, straight down `series_updated_idx`.
async fn fetch_page_by_recency(
    pool: &PgPool,
    filter: &SeriesFilter,
    query: Option<&str>,
) -> DbResult<Vec<FilteredRow>> {
    let rows = sqlx::query_as!(
            FilteredRow,
            "SELECT s.id, s.canonical_title, s.normalized_title, s.description, s.cover_url, \
                    s.content_type AS \"content_type: ContentType\", \
                    s.status AS \"status: SeriesStatus\", s.release_year, \
                    s.created_at, s.updated_at, \
                    (SELECT count(DISTINCT ss.provider_id) FROM series_sources ss WHERE ss.series_id = s.id) AS \"source_count!\" \
             FROM series s \
             WHERE ($1::text IS NULL OR s.normalized_title % $1 \
                     OR s.search_vec @@ plainto_tsquery('simple', $1) \
                     OR EXISTS (SELECT 1 FROM series_titles st \
                                WHERE st.series_id = s.id AND st.normalized % $1)) \
               AND ($2::content_type IS NULL OR s.content_type = $2) \
               AND ($3::series_status IS NULL OR s.status = $3) \
               AND ($4::int IS NULL OR s.release_year >= $4) \
               AND ($5::int IS NULL OR s.release_year <= $5) \
               AND ($6::text IS NULL OR EXISTS ( \
                     SELECT 1 FROM series_sources ss JOIN providers p ON p.id = ss.provider_id \
                     WHERE ss.series_id = s.id AND p.slug = $6)) \
               AND ($7::int IS NULL OR ( \
                     SELECT COALESCE(sum(ss.chapter_count),0) FROM series_sources ss \
                     WHERE ss.series_id = s.id) >= $7) \
               AND (cardinality($8::text[]) = 0 OR NOT EXISTS ( \
                     SELECT unnest($8::text[]) \
                     EXCEPT \
                     SELECT t.slug FROM series_tags stg JOIN tags t ON t.id = stg.tag_id \
                     WHERE stg.series_id = s.id)) \
               AND (cardinality($9::text[]) = 0 OR NOT EXISTS ( \
                     SELECT 1 FROM series_tags stg JOIN tags t ON t.id = stg.tag_id \
                     WHERE stg.series_id = s.id AND t.slug = ANY($9::text[]))) \
             ORDER BY s.updated_at DESC, s.id DESC \
             LIMIT $10 OFFSET $11",
            query,
            filter.content_type as Option<ContentType>,
            filter.status as Option<SeriesStatus>,
            filter.year_min,
            filter.year_max,
            filter.provider_slug.as_deref(),
            filter.min_chapters,
            &filter.tags as &[String],
            &filter.exclude_tags as &[String],
            filter.limit,
            filter.offset,
        )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Every other order, selected by the bound sort token.
///
/// Postgres constant-folds the inactive `CASE` branches, so `title` still reaches
/// `series_title_sort_idx` and the two correlated aggregates are never evaluated unless the
/// order that needs them was asked for.
async fn fetch_page_by_sort_token(
    pool: &PgPool,
    filter: &SeriesFilter,
    query: Option<&str>,
) -> DbResult<Vec<FilteredRow>> {
    let rows = sqlx::query_as!(
            FilteredRow,
            "SELECT s.id, s.canonical_title, s.normalized_title, s.description, s.cover_url, \
                    s.content_type AS \"content_type: ContentType\", \
                    s.status AS \"status: SeriesStatus\", s.release_year, \
                    s.created_at, s.updated_at, \
                    (SELECT count(DISTINCT ss.provider_id) FROM series_sources ss WHERE ss.series_id = s.id) AS \"source_count!\" \
             FROM series s \
             WHERE ($1::text IS NULL OR s.normalized_title % $1 \
                     OR s.search_vec @@ plainto_tsquery('simple', $1) \
                     OR EXISTS (SELECT 1 FROM series_titles st \
                                WHERE st.series_id = s.id AND st.normalized % $1)) \
               AND ($2::content_type IS NULL OR s.content_type = $2) \
               AND ($3::series_status IS NULL OR s.status = $3) \
               AND ($4::int IS NULL OR s.release_year >= $4) \
               AND ($5::int IS NULL OR s.release_year <= $5) \
               AND ($6::text IS NULL OR EXISTS ( \
                     SELECT 1 FROM series_sources ss JOIN providers p ON p.id = ss.provider_id \
                     WHERE ss.series_id = s.id AND p.slug = $6)) \
               AND ($7::int IS NULL OR ( \
                     SELECT COALESCE(sum(ss.chapter_count),0) FROM series_sources ss \
                     WHERE ss.series_id = s.id) >= $7) \
               AND (cardinality($8::text[]) = 0 OR NOT EXISTS ( \
                     SELECT unnest($8::text[]) \
                     EXCEPT \
                     SELECT t.slug FROM series_tags stg JOIN tags t ON t.id = stg.tag_id \
                     WHERE stg.series_id = s.id)) \
               AND (cardinality($9::text[]) = 0 OR NOT EXISTS ( \
                     SELECT 1 FROM series_tags stg JOIN tags t ON t.id = stg.tag_id \
                     WHERE stg.series_id = s.id AND t.slug = ANY($9::text[]))) \
             ORDER BY \
               CASE WHEN $12 = 'title' THEN s.canonical_title END ASC NULLS LAST, \
               CASE WHEN $12 = 'year' THEN s.release_year END DESC NULLS LAST, \
               CASE WHEN $12 = 'chapters' THEN ( \
                     SELECT COALESCE(sum(ss.chapter_count),0)::int8 FROM series_sources ss \
                     WHERE ss.series_id = s.id) END DESC NULLS LAST, \
               CASE WHEN $12 = 'sources' THEN ( \
                     SELECT count(DISTINCT ss.provider_id)::int8 FROM series_sources ss \
                     WHERE ss.series_id = s.id) END DESC NULLS LAST, \
               s.updated_at DESC, s.id DESC \
             LIMIT $10 OFFSET $11",
            query,
            filter.content_type as Option<ContentType>,
            filter.status as Option<SeriesStatus>,
            filter.year_min,
            filter.year_max,
            filter.provider_slug.as_deref(),
            filter.min_chapters,
            &filter.tags as &[String],
            &filter.exclude_tags as &[String],
            filter.limit,
            filter.offset,
            filter.sort.as_token(),
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// How many rows match the filter, ignoring `limit`/`offset`.
///
/// The predicate list is [`fetch_filtered_page`]'s and must stay that way — a total that
/// disagrees with the page is a pager offering a page that comes back empty.
async fn count_filtered(
    pool: &PgPool,
    filter: &SeriesFilter,
    query: Option<&str>,
) -> DbResult<i64> {
    let total = sqlx::query_scalar!(
        "SELECT count(*) AS \"total!\" FROM series s \
         WHERE ($1::text IS NULL OR s.normalized_title % $1 \
                 OR s.search_vec @@ plainto_tsquery('simple', $1) \
                 OR EXISTS (SELECT 1 FROM series_titles st \
                            WHERE st.series_id = s.id AND st.normalized % $1)) \
           AND ($2::content_type IS NULL OR s.content_type = $2) \
           AND ($3::series_status IS NULL OR s.status = $3) \
           AND ($4::int IS NULL OR s.release_year >= $4) \
           AND ($5::int IS NULL OR s.release_year <= $5) \
           AND ($6::text IS NULL OR EXISTS ( \
                 SELECT 1 FROM series_sources ss JOIN providers p ON p.id = ss.provider_id \
                 WHERE ss.series_id = s.id AND p.slug = $6)) \
           AND ($7::int IS NULL OR ( \
                 SELECT COALESCE(sum(ss.chapter_count),0) FROM series_sources ss \
                 WHERE ss.series_id = s.id) >= $7) \
           AND (cardinality($8::text[]) = 0 OR NOT EXISTS ( \
                 SELECT unnest($8::text[]) \
                 EXCEPT \
                 SELECT t.slug FROM series_tags stg JOIN tags t ON t.id = stg.tag_id \
                 WHERE stg.series_id = s.id)) \
           AND (cardinality($9::text[]) = 0 OR NOT EXISTS ( \
                 SELECT 1 FROM series_tags stg JOIN tags t ON t.id = stg.tag_id \
                 WHERE stg.series_id = s.id AND t.slug = ANY($9::text[])))",
        query,
        filter.content_type as Option<ContentType>,
        filter.status as Option<SeriesStatus>,
        filter.year_min,
        filter.year_max,
        filter.provider_slug.as_deref(),
        filter.min_chapters,
        &filter.tags as &[String],
        &filter.exclude_tags as &[String],
    )
    .fetch_one(pool)
    .await?;
    Ok(total)
}

/// Alternative titles of a series (design §9.2 enrichment). Empty when none are recorded.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. An unknown `series_id` and a
/// series with no synonyms recorded are the same empty `Vec`, not [`crate::DbError::NotFound`];
/// the series-detail handler has already established the series exists.
pub async fn list_series_titles<'e, E: PgExecutor<'e>>(
    exec: E,
    series_id: SeriesId,
) -> DbResult<Vec<String>> {
    let rows = sqlx::query_scalar!(
        "SELECT title FROM series_titles WHERE series_id = $1 ORDER BY title",
        series_id.as_uuid(),
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

/// Author/artist credits attached to a series, alphabetically (mirrors [`list_series_tags`]).
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. An unknown `series_id` and a
/// series with no credits are the same empty `Vec`, not [`crate::DbError::NotFound`] — an
/// uncredited series renders without the byline rather than failing the page.
pub async fn list_series_authors<'e, E: PgExecutor<'e>>(
    exec: E,
    series_id: SeriesId,
) -> DbResult<Vec<tankovault_domain::Author>> {
    #[derive(FromRow)]
    struct Row {
        id: Uuid,
        slug: String,
        name: String,
    }
    let rows = sqlx::query_as!(
        Row,
        "SELECT a.id, a.slug, a.name FROM series_authors sa JOIN authors a ON a.id = sa.author_id \
         WHERE sa.series_id = $1 ORDER BY a.name",
        series_id.as_uuid(),
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| tankovault_domain::Author {
            id: tankovault_domain::AuthorId::from_uuid(r.id),
            slug: r.slug,
            name: r.name,
        })
        .collect())
}

/// Tags attached to a series, alphabetically (design §9.2 enrichment).
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. An unknown `series_id` and an
/// untagged series are the same empty `Vec`, not [`crate::DbError::NotFound`].
pub async fn list_series_tags<'e, E: PgExecutor<'e>>(
    exec: E,
    series_id: SeriesId,
) -> DbResult<Vec<tankovault_domain::Tag>> {
    #[derive(FromRow)]
    struct Row {
        id: Uuid,
        slug: String,
        name: String,
    }
    let rows = sqlx::query_as!(
        Row,
        "SELECT t.id, t.slug, t.name FROM series_tags stg JOIN tags t ON t.id = stg.tag_id \
         WHERE stg.series_id = $1 ORDER BY t.name",
        series_id.as_uuid(),
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| tankovault_domain::Tag {
            id: tankovault_domain::TagId::from_uuid(r.id),
            slug: r.slug,
            name: r.name,
        })
        .collect())
}

/// List all tags/genres, alphabetically (design §11 `GET /v1/tags`).
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. A catalogue nothing has been
/// ingested into yet is an empty `Vec`. This one feeds the discover filter panel, so a failure
/// defaulted to an empty list would render a filter offering no tags — indistinguishable from a
/// fresh install, and it would look like the feature works.
pub async fn list_tags<'e, E: PgExecutor<'e>>(exec: E) -> DbResult<Vec<tankovault_domain::Tag>> {
    #[derive(FromRow)]
    struct Row {
        id: Uuid,
        slug: String,
        name: String,
    }
    let rows = sqlx::query_as!(Row, "SELECT id, slug, name FROM tags ORDER BY name")
        .fetch_all(exec)
        .await?;
    Ok(rows
        .into_iter()
        .map(|r| tankovault_domain::Tag {
            id: tankovault_domain::TagId::from_uuid(r.id),
            slug: r.slug,
            name: r.name,
        })
        .collect())
}
#[cfg(test)]
mod sort_tests {
    use super::SeriesSort;
    use std::str::FromStr as _;

    /// Every token the API accepts must round-trip, and an unknown one must be an error
    /// rather than a silent fallback.
    ///
    /// The bug this pins: `SeriesFilter::sort` was an `Option<String>` matched with a
    /// trailing `_ =>` arm, so `?sort=titel` returned a `200` ordered by recency. It also
    /// pins the token strings themselves, which are bound straight into the `ORDER BY`
    /// `CASE` expressions — renaming one there without renaming it here would silently
    /// disable that sort order.
    #[test]
    fn every_sort_token_round_trips_and_unknown_is_refused() {
        for sort in [
            SeriesSort::Updated,
            SeriesSort::Title,
            SeriesSort::Chapters,
            SeriesSort::Sources,
            SeriesSort::Year,
            SeriesSort::Rating,
        ] {
            assert_eq!(SeriesSort::from_str(sort.as_token()).unwrap(), sort);
        }
        assert!(SeriesSort::from_str("titel").is_err());
        assert!(SeriesSort::from_str("").is_err());
    }

    /// Only the two orders the dedicated, index-backed recency statement can serve may claim
    /// it. If `Title` ever answered `true` here it would be ordered by `updated_at` with no
    /// error anywhere.
    #[test]
    fn only_recency_orders_use_the_indexed_statement() {
        assert!(SeriesSort::Updated.is_recency());
        assert!(SeriesSort::Rating.is_recency());
        for sort in [
            SeriesSort::Title,
            SeriesSort::Chapters,
            SeriesSort::Sources,
            SeriesSort::Year,
        ] {
            assert!(
                !sort.is_recency(),
                "{} must not use recency",
                sort.as_token()
            );
        }
    }
}
