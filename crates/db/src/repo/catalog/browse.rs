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
/// [`crate::DbError::Sqlx`] only; no match is an empty `Vec`, not [`crate::DbError::NotFound`].
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
        // Matches canonical title, FTS vector, and alternative titles; ranks by best trigram
        // similarity across all three. The three predicates are a UNION of index-driven scans
        // rather than an `OR` chain: an `EXISTS` under `OR` cannot become a semi-join, which
        // costs the planner every index on `series` and leaves it scanning all of it (see
        // `crate::repo::matching::find_candidates`).
        sqlx::query_as!(
            ListRow,
            "WITH matched AS ( \
               SELECT s.id FROM series s WHERE s.normalized_title % $1 \
               UNION \
               SELECT s.id FROM series s WHERE s.search_vec @@ plainto_tsquery('simple', $1) \
               UNION \
               SELECT st.series_id FROM series_titles st WHERE st.normalized % $1 \
             ), ranked AS ( \
               SELECT s.id, s.canonical_title, s.normalized_title, s.description, s.cover_url, \
                      s.content_type, s.status, s.release_year, s.created_at, s.updated_at, \
                      GREATEST( \
                        similarity(s.normalized_title, $1), \
                        COALESCE((SELECT MAX(similarity(st.normalized, $1)) \
                                  FROM series_titles st WHERE st.series_id = s.id), 0) \
                      ) AS sim \
               FROM series s JOIN matched m ON m.id = s.id \
               ORDER BY sim DESC \
               LIMIT $2 \
             ) \
             SELECT r.id, r.canonical_title, r.normalized_title, r.description, r.cover_url, \
                    r.content_type AS \"content_type: ContentType\", \
                    r.status AS \"status: SeriesStatus\", r.release_year, \
                    r.created_at, r.updated_at, \
                    (SELECT count(DISTINCT ss.provider_id) FROM series_sources ss WHERE ss.series_id = r.id) AS \"source_count!\" \
             FROM ranked r \
             ORDER BY r.sim DESC",
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
/// A closed enum, not a passed-through string: an unrecognised token must be rejected (400),
/// not silently fall back to `updated`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SeriesSort {
    /// Default; the only order with a dedicated index (`series_updated_idx`).
    #[default]
    Updated,
    Title,
    Chapters,
    Sources,
    Year,
    /// No rating column yet; falls back to recency rather than refusing a token the frontend
    /// already offers.
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
/// Every field is optional; `None`/empty means "no constraint". Enum filters bind as native
/// Postgres types, not text — text binding both loses the index and lets an unparseable token
/// match nothing instead of being refused. `tags` requires all listed slugs; `exclude_tags`
/// excludes any.
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

#[derive(FromRow)]
struct CountRow {
    total: i64,
}

/// Every filtered-browse statement, built from one copy of the shared filter predicate.
///
/// The `sqlx` macros need their SQL as literal tokens at the call site, so a `const` or a
/// `concat!` is invisible to them — but `sqlx` 0.9 accepts a `"a" + "b"` chain of literals, and a
/// macro can hand it one. That is what keeps the predicate and the row projection written once
/// across the six statements below (recency/sort-token page × search/no-search, plus the two
/// counts) instead of six times; `crates/db/tests/repo_browse.rs` is the differential over what
/// still differs.
///
/// The predicate binds `$1`–`$8`, so a call site numbers its own parameters from `$9` up. `$cte`
/// and `$join` carry the search branch's matched-id set, `$tail` the ordering and paging.
macro_rules! browse_statement {
    (page $cte:literal, $join:literal, $tail:literal, $($args:tt)*) => {
        browse_statement!(
            @build FilteredRow,
            $cte,
            "s.id, s.canonical_title, s.normalized_title, s.description, s.cover_url, \
             s.content_type AS \"content_type: ContentType\", \
             s.status AS \"status: SeriesStatus\", s.release_year, \
             s.created_at, s.updated_at, \
             (SELECT count(DISTINCT ss.provider_id) FROM series_sources ss \
              WHERE ss.series_id = s.id) AS \"source_count!\"",
            $join,
            $tail,
            $($args)*
        )
    };
    (count $cte:literal, $join:literal, $($args:tt)*) => {
        browse_statement!(
            @build CountRow,
            $cte,
            "count(*) AS \"total!\"",
            $join,
            "",
            $($args)*
        )
    };
    (@build $row:path, $cte:literal, $projection:literal, $join:literal, $tail:literal,
     $($args:tt)*) => {
        sqlx::query_as!(
            $row,
            $cte
                + "SELECT " + $projection + " FROM series s" + $join
                + " WHERE ($1::content_type IS NULL OR s.content_type = $1) \
                     AND ($2::series_status IS NULL OR s.status = $2) \
                     AND ($3::int IS NULL OR s.release_year >= $3) \
                     AND ($4::int IS NULL OR s.release_year <= $4) \
                     AND ($5::text IS NULL OR EXISTS ( \
                           SELECT 1 FROM series_sources ss JOIN providers p ON p.id = ss.provider_id \
                           WHERE ss.series_id = s.id AND p.slug = $5)) \
                     AND ($6::int IS NULL OR ( \
                           SELECT COALESCE(sum(ss.chapter_count),0) FROM series_sources ss \
                           WHERE ss.series_id = s.id) >= $6) \
                     AND (cardinality($7::text[]) = 0 OR NOT EXISTS ( \
                           SELECT unnest($7::text[]) \
                           EXCEPT \
                           SELECT t.slug FROM series_tags stg JOIN tags t ON t.id = stg.tag_id \
                           WHERE stg.series_id = s.id)) \
                     AND (cardinality($8::text[]) = 0 OR NOT EXISTS ( \
                           SELECT 1 FROM series_tags stg JOIN tags t ON t.id = stg.tag_id \
                           WHERE stg.series_id = s.id AND t.slug = ANY($8::text[])))"
                + $tail,
            $($args)*
        )
    };
}

/// Query the browse list with server-side filtering, sorting and offset pagination
/// (frontend §9.1). Returns the page plus the total match count for the pager.
///
/// Two page statements, not five (drift risk across near-identical `ORDER BY` copies) or one
/// (folding every sort key into bound `CASE` expressions loses the default order's index under
/// a generic plan), each in a search and a no-search form — see [`fetch_filtered_page`] for why
/// the search term cannot be a bound `NULL` in one statement. The total is counted separately
/// from the page: a `count(*) OVER()` window forces full materialization before `LIMIT` applies,
/// where a concurrent index-only count does not. Takes `&PgPool`, not a generic executor, so the
/// two queries can run concurrently.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; a filter matching nothing is `{items: [], total: 0}`.
/// `try_join!` fails the pair if either statement fails; the count can drift from the page
/// under concurrent writes, which the pager tolerates by design.
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
/// Two branches, not one statement: the order picks the statement, and a search term picks
/// whether the matched-id set is joined in. The term cannot be folded back into a single
/// statement as `$1::text IS NULL OR … OR EXISTS (…)` — an `EXISTS` under `OR` cannot be pulled
/// up into a semi-join, so no index on `series` is usable and the planner scans all of it (see
/// [`crate::repo::matching::find_candidates`]). The `UNION` of index-driven scans that fixes
/// that has nothing to union when there is no term, which is why this branches rather than
/// rewrites.
///
/// Both page statements end with `s.id DESC` as a tiebreaker — without it, ties in the sort key
/// give adjacent `OFFSET` pages no stable order.
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
    let rows = if let Some(q) = query {
        browse_statement!(
            page "WITH matched AS ( \
                    SELECT s.id FROM series s WHERE s.normalized_title % $11 \
                    UNION \
                    SELECT s.id FROM series s \
                     WHERE s.search_vec @@ plainto_tsquery('simple', $11) \
                    UNION \
                    SELECT st.series_id FROM series_titles st WHERE st.normalized % $11 \
                  ) ",
            " JOIN matched m ON m.id = s.id",
            " ORDER BY s.updated_at DESC, s.id DESC LIMIT $9 OFFSET $10",
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
            q,
        )
        .fetch_all(pool)
        .await?
    } else {
        browse_statement!(
            page "",
            "",
            " ORDER BY s.updated_at DESC, s.id DESC LIMIT $9 OFFSET $10",
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
        .await?
    };
    Ok(rows)
}

/// Every other order, selected by the bound sort token.
///
/// Postgres constant-folds inactive `CASE` branches, so only the requested order's aggregates
/// evaluate.
async fn fetch_page_by_sort_token(
    pool: &PgPool,
    filter: &SeriesFilter,
    query: Option<&str>,
) -> DbResult<Vec<FilteredRow>> {
    let rows = if let Some(q) = query {
        browse_statement!(
            page "WITH matched AS ( \
                    SELECT s.id FROM series s WHERE s.normalized_title % $12 \
                    UNION \
                    SELECT s.id FROM series s \
                     WHERE s.search_vec @@ plainto_tsquery('simple', $12) \
                    UNION \
                    SELECT st.series_id FROM series_titles st WHERE st.normalized % $12 \
                  ) ",
            " JOIN matched m ON m.id = s.id",
            " ORDER BY \
                CASE WHEN $11 = 'title' THEN s.canonical_title END ASC NULLS LAST, \
                CASE WHEN $11 = 'year' THEN s.release_year END DESC NULLS LAST, \
                CASE WHEN $11 = 'chapters' THEN ( \
                      SELECT COALESCE(sum(ss.chapter_count),0)::int8 FROM series_sources ss \
                      WHERE ss.series_id = s.id) END DESC NULLS LAST, \
                CASE WHEN $11 = 'sources' THEN ( \
                      SELECT count(DISTINCT ss.provider_id)::int8 FROM series_sources ss \
                      WHERE ss.series_id = s.id) END DESC NULLS LAST, \
                s.updated_at DESC, s.id DESC \
              LIMIT $9 OFFSET $10",
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
            q,
        )
        .fetch_all(pool)
        .await?
    } else {
        browse_statement!(
            page "",
            "",
            " ORDER BY \
                CASE WHEN $11 = 'title' THEN s.canonical_title END ASC NULLS LAST, \
                CASE WHEN $11 = 'year' THEN s.release_year END DESC NULLS LAST, \
                CASE WHEN $11 = 'chapters' THEN ( \
                      SELECT COALESCE(sum(ss.chapter_count),0)::int8 FROM series_sources ss \
                      WHERE ss.series_id = s.id) END DESC NULLS LAST, \
                CASE WHEN $11 = 'sources' THEN ( \
                      SELECT count(DISTINCT ss.provider_id)::int8 FROM series_sources ss \
                      WHERE ss.series_id = s.id) END DESC NULLS LAST, \
                s.updated_at DESC, s.id DESC \
              LIMIT $9 OFFSET $10",
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
        .await?
    };
    Ok(rows)
}

/// How many rows match the filter, ignoring `limit`/`offset`.
///
/// Must take the same search branch as [`fetch_filtered_page`] and share its predicate, or the
/// pager can offer a page that comes back empty.
async fn count_filtered(
    pool: &PgPool,
    filter: &SeriesFilter,
    query: Option<&str>,
) -> DbResult<i64> {
    let row = if let Some(q) = query {
        browse_statement!(
            count "WITH matched AS ( \
                     SELECT s.id FROM series s WHERE s.normalized_title % $9 \
                     UNION \
                     SELECT s.id FROM series s \
                      WHERE s.search_vec @@ plainto_tsquery('simple', $9) \
                     UNION \
                     SELECT st.series_id FROM series_titles st WHERE st.normalized % $9 \
                   ) ",
            " JOIN matched m ON m.id = s.id",
            filter.content_type as Option<ContentType>,
            filter.status as Option<SeriesStatus>,
            filter.year_min,
            filter.year_max,
            filter.provider_slug.as_deref(),
            filter.min_chapters,
            &filter.tags as &[String],
            &filter.exclude_tags as &[String],
            q,
        )
        .fetch_one(pool)
        .await?
    } else {
        browse_statement!(
            count "",
            "",
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
        .await?
    };
    Ok(row.total)
}

/// Alternative titles of a series (design §9.2 enrichment). Empty when none are recorded.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; unknown id or no synonyms is the same empty `Vec`.
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
/// [`crate::DbError::Sqlx`] only; unknown id or no credits is the same empty `Vec`.
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
/// [`crate::DbError::Sqlx`] only; unknown id or untagged is the same empty `Vec`.
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
/// [`crate::DbError::Sqlx`] only; must not be defaulted to empty — that reads as "no tags"
/// rather than a failed fetch.
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

    /// Pins: `sort` used to be a passed-through string with a `_ =>` fallback, so
    /// `?sort=titel` silently returned recency order instead of 400. Also pins the token
    /// strings bound into the `ORDER BY CASE`.
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

    /// Pins which orders route to the indexed recency statement; a wrong answer here silently
    /// reorders results with no error.
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
