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
    /// The series row, with its cover already resolvable.
    pub series: Series,
    /// Providers carrying it.
    pub source_count: i64,
}

/// Query the browse list with keyset pagination on `(created_at, id)`.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; no match is an empty `Vec`, not [`crate::DbError::NotFound`].
pub async fn list_series<'e, E: PgExecutor<'e>>(
    exec: E,
    query: Option<&str>,
    include_adult: bool,
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
               WHERE NOT s.adult_gated OR $3 \
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
            include_adult,
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
             FROM series s WHERE NOT s.adult_gated OR $2 \
             ORDER BY s.updated_at DESC LIMIT $1",
            limit,
            include_adult,
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
    /// Default, and the order the recency statement walks an index for.
    #[default]
    Updated,
    /// Best match for the search term. Meaningless without one, and degrades to [`Self::Updated`]
    /// when there is none rather than refusing — the sort control is shared with the browse grid.
    Relevance,
    /// Canonical title, A to Z.
    Title,
    /// Most chapters first.
    Chapters,
    /// Most providers first.
    Sources,
    /// Newest release year first.
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
            Self::Relevance => "relevance",
            Self::Title => "title",
            Self::Chapters => "chapters",
            Self::Sources => "sources",
            Self::Year => "year",
            Self::Rating => "rating",
        }
    }

    /// Whether this order is served by the dedicated recency statement.
    ///
    /// [`Self::Relevance`] is in the list because this is only consulted once the relevance
    /// branch has already declined — which it does when there is no search term to rank by.
    fn is_recency(self) -> bool {
        matches!(self, Self::Updated | Self::Rating | Self::Relevance)
    }
}

impl std::str::FromStr for SeriesSort {
    type Err = ParseSeriesSortError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "updated" => Ok(Self::Updated),
            "relevance" => Ok(Self::Relevance),
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
    /// Free text over the search vector and the title trigrams.
    pub query: Option<String>,
    /// Keep only this medium.
    pub content_type: Option<ContentType>,
    /// Keep only this publication status.
    pub status: Option<SeriesStatus>,
    /// Keep only series carried by this provider.
    pub provider_slug: Option<String>,
    /// Keep only series carrying every one of these slugs.
    pub tags: Vec<String>,
    /// Drop series carrying any of these slugs.
    pub exclude_tags: Vec<String>,
    /// Inclusive lower bound on the release year.
    pub year_min: Option<i32>,
    /// Inclusive upper bound on it.
    pub year_max: Option<i32>,
    /// Inclusive lower bound on the summed chapter count.
    pub min_chapters: Option<i32>,
    /// Whether adult-gated series may appear. `false` — the `Default` — is the safe value, and
    /// the only correct one for a caller with no authenticated reader to ask.
    ///
    /// Not a filter the client picks: the API resolves it from the caller's stored opt-in and
    /// the deployment flag. A request parameter here would be an age gate anybody could open by
    /// editing a query string.
    pub include_adult: bool,
    /// The reader whose watchlist [`Self::tracked`] is about. `None` — the `Default` — switches
    /// the tracking filter off entirely, which is the only correct value for a caller with no
    /// authenticated reader behind it.
    pub tracked_by: Option<uuid::Uuid>,
    /// `Some(true)` keeps only series that reader tracks, `Some(false)` only ones they do not,
    /// `None` neither. Ignored without [`Self::tracked_by`].
    pub tracked: Option<bool>,
    /// Which key to order the grid on.
    pub sort: SeriesSort,
    /// Rows per page.
    pub limit: i64,
    /// Rows to skip.
    pub offset: i64,
}

/// Whether a browse request also counts every matching row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Total {
    /// Count the rows the filter matches.
    Count,
    /// Skip the count; [`SeriesPage::total`] is `None`. For a caller that already holds the total
    /// for this filter, or never shows one.
    Skip,
}

/// A page of the filtered browse list, whether another page follows, and the total when asked.
pub struct SeriesPage {
    /// The page, in the filter's order.
    pub items: Vec<SeriesListItem>,
    /// Rows the filter matched, ignoring `limit` and `offset`; `None` under [`Total::Skip`].
    pub total: Option<i64>,
    /// Whether at least one more row follows this page.
    pub has_more: bool,
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
    /// Rows a search matched, carried on every row of a search page; `None` without a search.
    window_total: Option<i64>,
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
/// across the statements below; `crates/db/tests/repo_browse.rs` is the differential over what
/// still differs.
///
/// The predicate reads `series_browse` (migration 0060), the narrow projection holding every
/// filter and sort key, and binds `$1`–`$11`, so a call site numbers its own parameters from `$12`
/// up. `$cte` and `$join` carry the search branch's matched-id set.
///
/// `$9` is the adult gate, and it lives in the shared predicate rather than at the call sites
/// precisely because there are several: a gate that has to be remembered at every call site is a
/// gate that will be missing from the next statement somebody adds. `$10`/`$11` are the tracking
/// filter, there for the same reason and for one more: the count and the page have to agree about
/// it, or the pager offers a page that comes back empty.
///
/// # Why the include-tags filter maps slugs to ids with a placeholder
///
/// `tag_ids @> ARRAY(SELECT id FROM tags WHERE slug = ANY($7))` silently drops a slug no tag has,
/// so `tag=romance&tag=typo` would return every romance series instead of none. Each requested
/// slug maps to its id or to the all-zero UUID, which no row carries.
///
/// # Why `max_chapters` is a `max`, not a `sum`
///
/// `series_sources.chapter_count` is *one source's* scanned-row count, and a series carried by
/// several sources — which is what every merge leaves behind — has one row per carrier. Summed,
/// a 200-chapter work mirrored on three sites filtered and sorted as a 600-chapter one while the
/// card beside it read 200. `max` is the richest carrier's count: exact whenever one source's
/// catalogue covers the others', and never an over-claim.
macro_rules! browse_statement {
    // Order and limit on the projection alone, then read only the page's rows from `series`.
    (narrow $cte:literal, $join:literal, $window:literal, $keys:literal, $order:literal,
     $outer_order:literal, $($args:tt)*) => {
        browse_statement!(
            @build FilteredRow,
            [$cte,
             "SELECT s.id, s.canonical_title, s.normalized_title, s.description, s.cover_url, \
                     s.content_type AS \"content_type: ContentType\", \
                     s.status AS \"status: SeriesStatus\", s.release_year, \
                     s.created_at, s.updated_at, \
                     p.source_count::int8 AS \"source_count!\", \
                     p.window_total AS \"window_total?\" \
              FROM (SELECT sb.series_id, sb.updated_at, sb.source_count, ",
             $window, " AS window_total", $keys, " FROM series_browse sb", $join],
            [$order, ") p JOIN series s ON s.id = p.series_id", $outer_order],
            $($args)*
        )
    };
    // The relevance order ranks on `series` columns, so it joins before ordering.
    (wide $cte:literal, $join:literal, $tail:literal, $($args:tt)*) => {
        browse_statement!(
            @build FilteredRow,
            [$cte,
             "SELECT s.id, s.canonical_title, s.normalized_title, s.description, s.cover_url, \
                     s.content_type AS \"content_type: ContentType\", \
                     s.status AS \"status: SeriesStatus\", s.release_year, \
                     s.created_at, s.updated_at, \
                     sb.source_count::int8 AS \"source_count!\", \
                     count(*) OVER () AS \"window_total?\" \
              FROM series_browse sb JOIN series s ON s.id = sb.series_id",
             $join],
            [$tail],
            $($args)*
        )
    };
    (count $cte:literal, $join:literal, $($args:tt)*) => {
        browse_statement!(
            @build CountRow,
            [$cte, "SELECT count(*) AS \"total!\" FROM series_browse sb", $join],
            [""],
            $($args)*
        )
    };
    (@build $row:path, [$($head:literal),+], [$($tail:literal),+], $($args:tt)*) => {
        sqlx::query_as!(
            $row,
            $($head +)+
                " WHERE ($1::content_type IS NULL OR sb.content_type = $1) \
                    AND ($2::series_status IS NULL OR sb.status = $2) \
                    AND ($3::int IS NULL OR sb.release_year >= $3) \
                    AND ($4::int IS NULL OR sb.release_year <= $4) \
                    AND ($5::text IS NULL OR sb.provider_ids @> ARRAY[COALESCE( \
                          (SELECT p.id FROM providers p WHERE p.slug = $5), \
                          '00000000-0000-0000-0000-000000000000'::uuid)]) \
                    AND ($6::int IS NULL OR sb.max_chapters >= $6) \
                    AND (cardinality($7::text[]) = 0 OR sb.tag_ids @> ARRAY( \
                          SELECT COALESCE(t.id, '00000000-0000-0000-0000-000000000000'::uuid) \
                          FROM unnest($7::text[]) AS x(slug) LEFT JOIN tags t ON t.slug = x.slug)) \
                    AND (cardinality($8::text[]) = 0 OR NOT sb.tag_ids && ARRAY( \
                          SELECT t.id FROM tags t WHERE t.slug = ANY($8::text[]))) \
                    AND (NOT sb.adult_gated OR $9) \
                    AND ($10::uuid IS NULL OR $11::bool IS NULL OR $11 = EXISTS ( \
                          SELECT 1 FROM watchlist_entries w \
                          WHERE w.series_id = sb.series_id AND w.user_id = $10))"
                $(+ $tail)+,
            $($args)*
        )
    };
}

/// Query the browse list with server-side filtering, sorting and offset pagination
/// (frontend §9.1). Returns the page, whether another follows, and the total under
/// [`Total::Count`].
///
/// A page reads `limit + 1` rows, so whether another page follows never depends on a count. A
/// search page carries its total as a window count over the matched set, which it has to build
/// anyway; only a search page past the end, which has no row to carry it, counts separately.
/// Without a search the count is its own statement, concurrent with the page: a window there would
/// materialise every matching row before the recency index's `LIMIT` could stop the scan.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; a filter matching nothing is an empty page with total `0`.
/// `try_join!` fails the pair if either statement fails; the count can drift from the page
/// under concurrent writes, which the pager tolerates by design.
pub async fn list_series_filtered(
    pool: &PgPool,
    filter: &SeriesFilter,
    total: Total,
) -> DbResult<SeriesPage> {
    let query = filter
        .query
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty());

    let (mut rows, mut count) = match (query, total) {
        (None, Total::Count) => {
            let (rows, count) = tokio::try_join!(
                fetch_filtered_page(pool, filter, None),
                count_filtered(pool, filter, None),
            )?;
            (rows, Some(count))
        }
        _ => (fetch_filtered_page(pool, filter, query).await?, None),
    };
    if let (Some(q), Total::Count) = (query, total) {
        count = match rows.first() {
            Some(row) => row.window_total,
            None if filter.offset == 0 => Some(0),
            None => Some(count_filtered(pool, filter, Some(q)).await?),
        };
    }

    let page_len = usize::try_from(filter.limit).unwrap_or(0);
    let has_more = rows.len() > page_len;
    rows.truncate(page_len);

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

    Ok(SeriesPage {
        items,
        total: count,
        has_more,
    })
}

/// One page of matching rows plus one, in the requested order.
///
/// Three branches, not one statement: the order picks the statement, and a search term picks
/// whether the matched-id set is joined in. The term cannot be folded back into a single
/// statement as `$1::text IS NULL OR … OR EXISTS (…)` — an `EXISTS` under `OR` cannot be pulled
/// up into a semi-join, so no index is usable and the planner scans everything (see
/// [`crate::repo::matching::find_candidates`]). The `UNION` of index-driven scans that fixes
/// that has nothing to union when there is no term, which is why this branches rather than
/// rewrites.
///
/// Every page statement ends with the series id as a tiebreaker — without it, ties in the sort key
/// give adjacent `OFFSET` pages no stable order.
async fn fetch_filtered_page(
    pool: &PgPool,
    filter: &SeriesFilter,
    query: Option<&str>,
) -> DbResult<Vec<FilteredRow>> {
    let fetch = filter.limit.saturating_add(1);
    match (filter.sort, query) {
        (SeriesSort::Relevance, Some(q)) => fetch_page_by_relevance(pool, filter, fetch, q).await,
        (sort, _) if sort.is_recency() => fetch_page_by_recency(pool, filter, fetch, query).await,
        _ => fetch_page_by_sort_token(pool, filter, fetch, query).await,
    }
}

/// Best-match order for a search term.
///
/// Its own statement rather than another `ORDER BY CASE` arm on [`fetch_page_by_sort_token`],
/// because the ranking needs the term itself and that statement's tail is shared with the
/// no-search branch, which has no term to bind.
///
/// The tiers exist because trigram similarity alone does not put an exact title first: a short
/// query is a large fraction of a short unrelated title, so `similarity()` happily ranks
/// "Berserk of Gluttony" above "Berserk". Exactness is therefore decided *before* similarity is
/// consulted — the canonical title first, then any alternative title, then a prefix. Prefix is
/// tested with `left(…)` rather than `LIKE $n || '%'` so a `%` or `_` surviving normalization
/// cannot turn the reader's query into a wildcard.
async fn fetch_page_by_relevance(
    pool: &PgPool,
    filter: &SeriesFilter,
    fetch: i64,
    query: &str,
) -> DbResult<Vec<FilteredRow>> {
    // The exactness tiers compare against `normalized_title`, so the term has to be reduced by
    // the same function that produced that column — otherwise "SPY×FAMILY" never equals the row
    // it is stored as.
    let key = tankovault_domain::normalize_title(query);
    let rows = browse_statement!(
        wide "WITH matched AS ( \
                SELECT s.id FROM series s WHERE s.normalized_title % $14 \
                UNION \
                SELECT s.id FROM series s \
                 WHERE s.search_vec @@ plainto_tsquery('simple', $14) \
                UNION \
                SELECT st.series_id FROM series_titles st WHERE st.normalized % $14 \
              ) ",
        " JOIN matched m ON m.id = sb.series_id",
        " ORDER BY \
            (s.normalized_title = $15) DESC, \
            EXISTS (SELECT 1 FROM series_titles st \
                     WHERE st.series_id = s.id AND st.normalized = $15) DESC, \
            (left(s.normalized_title, length($15)) = $15) DESC, \
            GREATEST( \
              similarity(s.normalized_title, $14), \
              COALESCE((SELECT max(similarity(st.normalized, $14)) \
                        FROM series_titles st WHERE st.series_id = s.id), 0) \
            ) DESC, \
            s.updated_at DESC, s.id DESC \
          LIMIT $12 OFFSET $13",
        filter.content_type as Option<ContentType>,
        filter.status as Option<SeriesStatus>,
        filter.year_min,
        filter.year_max,
        filter.provider_slug.as_deref(),
        filter.min_chapters,
        &filter.tags as &[String],
        &filter.exclude_tags as &[String],
        filter.include_adult,
        filter.tracked_by,
        filter.tracked,
        fetch,
        filter.offset,
        query,
        key,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// The default order: newest-updated first, straight down `series_browse_updated_idx`.
async fn fetch_page_by_recency(
    pool: &PgPool,
    filter: &SeriesFilter,
    fetch: i64,
    query: Option<&str>,
) -> DbResult<Vec<FilteredRow>> {
    let rows = if let Some(q) = query {
        browse_statement!(
            narrow "WITH matched AS ( \
                      SELECT s.id FROM series s WHERE s.normalized_title % $14 \
                      UNION \
                      SELECT s.id FROM series s \
                       WHERE s.search_vec @@ plainto_tsquery('simple', $14) \
                      UNION \
                      SELECT st.series_id FROM series_titles st WHERE st.normalized % $14 \
                    ) ",
            " JOIN matched m ON m.id = sb.series_id",
            "count(*) OVER ()",
            "",
            " ORDER BY sb.updated_at DESC, sb.series_id DESC LIMIT $12 OFFSET $13",
            " ORDER BY p.updated_at DESC, p.series_id DESC",
            filter.content_type as Option<ContentType>,
            filter.status as Option<SeriesStatus>,
            filter.year_min,
            filter.year_max,
            filter.provider_slug.as_deref(),
            filter.min_chapters,
            &filter.tags as &[String],
            &filter.exclude_tags as &[String],
            filter.include_adult,
            filter.tracked_by,
            filter.tracked,
            fetch,
            filter.offset,
            q,
        )
        .fetch_all(pool)
        .await?
    } else {
        browse_statement!(
            narrow "",
            "",
            "NULL::int8",
            "",
            " ORDER BY sb.updated_at DESC, sb.series_id DESC LIMIT $12 OFFSET $13",
            " ORDER BY p.updated_at DESC, p.series_id DESC",
            filter.content_type as Option<ContentType>,
            filter.status as Option<SeriesStatus>,
            filter.year_min,
            filter.year_max,
            filter.provider_slug.as_deref(),
            filter.min_chapters,
            &filter.tags as &[String],
            &filter.exclude_tags as &[String],
            filter.include_adult,
            filter.tracked_by,
            filter.tracked,
            fetch,
            filter.offset,
        )
        .fetch_all(pool)
        .await?
    };
    Ok(rows)
}

/// Every other order, selected by the bound sort token.
///
/// A custom plan folds the inactive `CASE` arms away, so the requested order is a plain key the
/// matching `series_browse_*_idx` index serves. The keys are selected so the outer lookup can
/// restore the order after the join.
async fn fetch_page_by_sort_token(
    pool: &PgPool,
    filter: &SeriesFilter,
    fetch: i64,
    query: Option<&str>,
) -> DbResult<Vec<FilteredRow>> {
    let rows = if let Some(q) = query {
        browse_statement!(
            narrow "WITH matched AS ( \
                      SELECT s.id FROM series s WHERE s.normalized_title % $15 \
                      UNION \
                      SELECT s.id FROM series s \
                       WHERE s.search_vec @@ plainto_tsquery('simple', $15) \
                      UNION \
                      SELECT st.series_id FROM series_titles st WHERE st.normalized % $15 \
                    ) ",
            " JOIN matched m ON m.id = sb.series_id",
            "count(*) OVER ()",
            ", CASE WHEN $14 = 'title' THEN sb.canonical_title END AS k_title, \
               CASE WHEN $14 = 'year' THEN sb.release_year END AS k_year, \
               CASE WHEN $14 = 'chapters' THEN sb.max_chapters END AS k_chapters, \
               CASE WHEN $14 = 'sources' THEN sb.source_count END AS k_sources",
            " ORDER BY \
                CASE WHEN $14 = 'title' THEN sb.canonical_title END ASC NULLS LAST, \
                CASE WHEN $14 = 'year' THEN sb.release_year END DESC NULLS LAST, \
                CASE WHEN $14 = 'chapters' THEN sb.max_chapters END DESC NULLS LAST, \
                CASE WHEN $14 = 'sources' THEN sb.source_count END DESC NULLS LAST, \
                sb.updated_at DESC, sb.series_id DESC \
              LIMIT $12 OFFSET $13",
            " ORDER BY p.k_title ASC NULLS LAST, p.k_year DESC NULLS LAST, \
                       p.k_chapters DESC NULLS LAST, p.k_sources DESC NULLS LAST, \
                       p.updated_at DESC, p.series_id DESC",
            filter.content_type as Option<ContentType>,
            filter.status as Option<SeriesStatus>,
            filter.year_min,
            filter.year_max,
            filter.provider_slug.as_deref(),
            filter.min_chapters,
            &filter.tags as &[String],
            &filter.exclude_tags as &[String],
            filter.include_adult,
            filter.tracked_by,
            filter.tracked,
            fetch,
            filter.offset,
            filter.sort.as_token(),
            q,
        )
        .fetch_all(pool)
        .await?
    } else {
        browse_statement!(
            narrow "",
            "",
            "NULL::int8",
            ", CASE WHEN $14 = 'title' THEN sb.canonical_title END AS k_title, \
               CASE WHEN $14 = 'year' THEN sb.release_year END AS k_year, \
               CASE WHEN $14 = 'chapters' THEN sb.max_chapters END AS k_chapters, \
               CASE WHEN $14 = 'sources' THEN sb.source_count END AS k_sources",
            " ORDER BY \
                CASE WHEN $14 = 'title' THEN sb.canonical_title END ASC NULLS LAST, \
                CASE WHEN $14 = 'year' THEN sb.release_year END DESC NULLS LAST, \
                CASE WHEN $14 = 'chapters' THEN sb.max_chapters END DESC NULLS LAST, \
                CASE WHEN $14 = 'sources' THEN sb.source_count END DESC NULLS LAST, \
                sb.updated_at DESC, sb.series_id DESC \
              LIMIT $12 OFFSET $13",
            " ORDER BY p.k_title ASC NULLS LAST, p.k_year DESC NULLS LAST, \
                       p.k_chapters DESC NULLS LAST, p.k_sources DESC NULLS LAST, \
                       p.updated_at DESC, p.series_id DESC",
            filter.content_type as Option<ContentType>,
            filter.status as Option<SeriesStatus>,
            filter.year_min,
            filter.year_max,
            filter.provider_slug.as_deref(),
            filter.min_chapters,
            &filter.tags as &[String],
            &filter.exclude_tags as &[String],
            filter.include_adult,
            filter.tracked_by,
            filter.tracked,
            fetch,
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
                     SELECT s.id FROM series s WHERE s.normalized_title % $12 \
                     UNION \
                     SELECT s.id FROM series s \
                      WHERE s.search_vec @@ plainto_tsquery('simple', $12) \
                     UNION \
                     SELECT st.series_id FROM series_titles st WHERE st.normalized % $12 \
                   ) ",
            " JOIN matched m ON m.id = sb.series_id",
            filter.content_type as Option<ContentType>,
            filter.status as Option<SeriesStatus>,
            filter.year_min,
            filter.year_max,
            filter.provider_slug.as_deref(),
            filter.min_chapters,
            &filter.tags as &[String],
            &filter.exclude_tags as &[String],
            filter.include_adult,
            filter.tracked_by,
            filter.tracked,
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
            filter.include_adult,
            filter.tracked_by,
            filter.tracked,
        )
        .fetch_one(pool)
        .await?
    };
    Ok(row.total)
}

/// What one projection verification batch found.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProjectionCheck {
    /// Series compared.
    pub checked: u64,
    /// Series whose projection row was missing.
    pub missing: u64,
    /// Series whose projection row disagreed with a recomputation.
    pub stale: u64,
    /// The last series compared, to resume after; `None` once the batch reached the end.
    pub resume_after: Option<Uuid>,
}

impl ProjectionCheck {
    /// Series that were missing or disagreed.
    #[must_use]
    pub const fn drifted(&self) -> u64 {
        self.missing + self.stale
    }
}

/// Compare the `series_browse` rows of up to `limit` series after `after` (in id order) with a
/// recomputation from the base tables, rebuild the ones that disagree, and report what was found.
///
/// A row a concurrent write changes between the comparison and the rebuild is recomputed under its
/// row lock, so the rebuild never stores an older answer than the trigger did.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn verify_projection(
    pool: &PgPool,
    after: Option<Uuid>,
    limit: i64,
) -> DbResult<ProjectionCheck> {
    let rows = sqlx::query!(
        "WITH batch AS ( \
             SELECT id FROM series WHERE $1::uuid IS NULL OR id > $1 ORDER BY id LIMIT $2 \
         ), live AS ( \
             SELECT s.id AS series_id, s.updated_at, s.content_type, s.status, s.release_year, \
                    s.adult_gated, s.canonical_title, \
                    COALESCE(src.max_chapters, 0) AS max_chapters, \
                    COALESCE(src.source_count, 0) AS source_count, \
                    COALESCE(src.provider_ids, '{}') AS provider_ids, \
                    COALESCE(tg.tag_ids, '{}') AS tag_ids \
             FROM batch b JOIN series s ON s.id = b.id \
             LEFT JOIN LATERAL ( \
                 SELECT max(chapter_count) AS max_chapters, \
                        count(DISTINCT provider_id)::int AS source_count, \
                        array_agg(DISTINCT provider_id ORDER BY provider_id) AS provider_ids \
                 FROM series_sources WHERE series_id = s.id) src ON true \
             LEFT JOIN LATERAL ( \
                 SELECT array_agg(tag_id ORDER BY tag_id) AS tag_ids \
                 FROM series_tags WHERE series_id = s.id) tg ON true \
         ) \
         SELECT l.series_id AS \"series_id!\", \
                (sb.series_id IS NULL) AS \"missing!\", \
                (sb.series_id IS NOT NULL AND \
                 (sb.updated_at, sb.content_type, sb.status, sb.release_year, sb.adult_gated, \
                  sb.canonical_title, sb.max_chapters, sb.source_count, sb.provider_ids, sb.tag_ids) \
                 IS DISTINCT FROM \
                 (l.updated_at, l.content_type, l.status, l.release_year, l.adult_gated, \
                  l.canonical_title, l.max_chapters, l.source_count, l.provider_ids, l.tag_ids)) \
                  AS \"stale!\" \
         FROM live l LEFT JOIN series_browse sb ON sb.series_id = l.series_id \
         ORDER BY l.series_id",
        after,
        limit,
    )
    .fetch_all(pool)
    .await?;

    let mut check = ProjectionCheck {
        checked: rows.len() as u64,
        resume_after: rows.last().map(|r| r.series_id),
        ..ProjectionCheck::default()
    };
    if i64::try_from(rows.len()).unwrap_or(i64::MAX) < limit {
        check.resume_after = None;
    }
    let mut rebuild = Vec::new();
    for row in &rows {
        check.missing += u64::from(row.missing);
        check.stale += u64::from(row.stale);
        if row.missing || row.stale {
            rebuild.push(row.series_id);
        }
    }
    if !rebuild.is_empty() {
        sqlx::query!("SELECT series_browse_rebuild($1)", &rebuild)
            .fetch_one(pool)
            .await?;
    }
    Ok(check)
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

/// Tag names for a set of series, alphabetically within each — the batched counterpart to
/// [`list_series_tags`], for card grids that would otherwise issue one query per cover.
///
/// Names, not [`tankovault_domain::Tag`]s: a card labels a series, it does not link the facet,
/// and shipping ids and slugs a caller cannot use is payload nobody reads.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only. An untagged series is absent from the map rather than present
/// as an empty `Vec`.
pub async fn tags_for_series<'e, E: PgExecutor<'e>>(
    exec: E,
    series_ids: &[SeriesId],
) -> DbResult<std::collections::HashMap<SeriesId, Vec<String>>> {
    #[derive(FromRow)]
    struct Row {
        series_id: Uuid,
        name: String,
    }
    let ids: Vec<Uuid> = series_ids.iter().map(|s| s.as_uuid()).collect();
    let rows = sqlx::query_as!(
        Row,
        "SELECT stg.series_id, t.name FROM series_tags stg JOIN tags t ON t.id = stg.tag_id \
         WHERE stg.series_id = ANY($1) ORDER BY stg.series_id, t.name",
        &ids,
    )
    .fetch_all(exec)
    .await?;
    let mut out: std::collections::HashMap<SeriesId, Vec<String>> =
        std::collections::HashMap::new();
    for row in rows {
        out.entry(SeriesId::from_uuid(row.series_id))
            .or_default()
            .push(row.name);
    }
    Ok(out)
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

/// A tag plus how much of the catalogue carries it.
pub struct TagFacet {
    /// The tag.
    pub tag: tankovault_domain::Tag,
    /// Series carrying it, under whatever filter the facet was computed with.
    pub series_count: i64,
}

/// Every tag with its series count, commonest first.
///
/// Popularity order, not alphabetical: a facet panel can only render so many chips, and an
/// alphabetical truncation cuts the list at whatever letter the cap lands on — which is how a
/// panel ends mid-alphabet and hides the genres most of the catalogue is actually tagged with.
/// Ties break on name so the order is stable between requests.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; must not be defaulted to empty — that reads as "no tags"
/// rather than a failed fetch.
pub async fn list_tag_facets<'e, E: PgExecutor<'e>>(exec: E) -> DbResult<Vec<TagFacet>> {
    #[derive(FromRow)]
    struct Row {
        id: Uuid,
        slug: String,
        name: String,
        series_count: i64,
    }
    // One aggregate over `series_tags` joined in, not a correlated `count(*)` per tag. The
    // correlated form is charged once per row of `tags` and estimated at 262 000 on the plan
    // audit's fixture — the whole vocabulary re-counted for a facet the panel loads on every
    // visit. This groups `series_tags` once and hash-joins it, which is the same answer for a
    // fraction of the work.
    let rows = sqlx::query_as!(
        Row,
        "SELECT t.id, t.slug, t.name, COALESCE(c.n, 0) AS \"series_count!\" \
         FROM tags t \
         LEFT JOIN (SELECT stg.tag_id, count(*) AS n FROM series_tags stg GROUP BY stg.tag_id) c \
                ON c.tag_id = t.id \
         ORDER BY COALESCE(c.n, 0) DESC, t.name",
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| TagFacet {
            tag: tankovault_domain::Tag {
                id: tankovault_domain::TagId::from_uuid(r.id),
                slug: r.slug,
                name: r.name,
            },
            series_count: r.series_count,
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
            SeriesSort::Relevance,
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
    ///
    /// `Relevance` belongs to that set: the relevance statement is chosen ahead of this check and
    /// only when a search term exists, so what this pins is the *fallback* — a relevance request
    /// with nothing to rank must land on recency, not on the sort-token statement whose `CASE`
    /// arms all miss and which would then order by `updated_at` the slow way.
    #[test]
    fn only_recency_orders_use_the_indexed_statement() {
        assert!(SeriesSort::Updated.is_recency());
        assert!(SeriesSort::Rating.is_recency());
        assert!(SeriesSort::Relevance.is_recency());
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
