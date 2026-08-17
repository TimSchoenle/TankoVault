//! The operator's catalogue maintenance reads and the destructive writes behind them: the
//! series list an operator triages from, the deployment-wide totals, bulk deletion, and the
//! purge.
//!
//! Everything here deletes through `series`/`chapters` rather than truncating, so the declared
//! `ON DELETE` action decides what happens to each dependant. That is the whole reason this
//! module does not use `TRUNCATE … CASCADE`: cascade ignores the action and would also empty
//! `sync_decisions`, `sync_history` and `sync_remote_entries`, which are journals of what the
//! system *did* and are meant to outlive the rows they mention — their `series_id` is
//! `ON DELETE SET NULL` precisely so they can.
//!
//! A purge empties the *catalogue*; it deliberately leaves the `tags` and `authors`
//! vocabularies standing. Neither is reachable from `series` by a foreign key, both are small,
//! and the next scan reuses their slugs — so deleting them would be extra destruction for no
//! operator benefit. Nothing here reports their size, for the same reason.
//!
//! "How many chapters does this series have" is answered by `series_sources.chapter_count`
//! throughout, never by counting `chapters` rows. That is the counter the browse filters and the
//! series page already read, and a maintenance filter that disagreed with the number in the
//! column beside it would be worse than one built on a denormalised value.

use crate::error::DbResult;
use sqlx::{Connection as _, FromRow, PgConnection, PgExecutor, PgPool};
use time::OffsetDateTime;
use uuid::Uuid;

/// Which rows a maintenance listing is narrowed to.
///
/// Named states rather than free-form predicates: these are what an operator actually hunts for
/// when they open this panel, and each is a shape that produces junk — a series no provider
/// carries any more, or one carried but never populated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SeriesHealth {
    #[default]
    Any,
    /// No `series_sources` row at all: nothing will ever scan it again.
    Orphaned,
    /// Sourced, but no chapter has ever been discovered.
    Empty,
}

/// What the maintenance listing is asking for.
#[derive(Debug, Clone, Default)]
pub struct MaintenanceFilter {
    /// Case-insensitive substring of the canonical title. Empty lists everything.
    pub search: String,
    /// Restrict to series carried by this provider slug.
    pub provider_slug: Option<String>,
    pub health: SeriesHealth,
    pub limit: i64,
    pub offset: i64,
}

/// One row of the maintenance list: what the series is, and how much would go with it.
#[derive(Debug, Clone)]
pub struct MaintenanceRow {
    pub id: Uuid,
    pub canonical_title: String,
    pub content_type: String,
    pub status: String,
    pub release_year: Option<i32>,
    /// Slugs of every provider carrying this series, so the operator can see what a deletion
    /// would have to be re-scanned from.
    pub providers: Vec<String>,
    pub source_count: i64,
    pub chapter_count: i64,
    /// Readers with this series on a watchlist — the blast radius that is not re-scannable.
    pub watcher_count: i64,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// A page of the maintenance list plus the unfiltered-by-page total.
pub struct MaintenancePage {
    pub items: Vec<MaintenanceRow>,
    pub total: i64,
}

/// The deployment-wide totals the purge panel states its blast radius from.
#[derive(Debug, Clone, FromRow)]
pub struct CatalogueTotals {
    pub series_total: i64,
    pub sources_total: i64,
    pub chapters_total: i64,
    /// Series with no source row left.
    pub orphaned_series: i64,
    /// Series with a source but no chapter.
    pub empty_series: i64,
    /// Watchlist entries that a full purge would take with it.
    pub watchlist_entries: i64,
    /// Reading positions that a full purge would take with it.
    pub progress_rows: i64,
}

/// What a deletion actually removed, per table.
///
/// Counted before the delete rather than derived from `rows_affected`, which only ever reports
/// the rows the statement named and says nothing about what cascaded.
#[derive(Debug, Clone, Copy, Default, FromRow)]
pub struct DeletionReport {
    pub series: i64,
    pub sources: i64,
    pub chapters: i64,
    pub watchlist_entries: i64,
    pub progress_rows: i64,
}

/// List the catalogue for maintenance, newest first.
///
/// Ordered by `created_at DESC` and deliberately not sortable: the panel narrows with the health
/// filter rather than by sorting a fifty-thousand-row list, and a bound sort key would put a
/// `CASE` in the `ORDER BY` that no index can serve.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; a filter matching nothing is `{items: [], total: 0}`.
pub async fn list_for_maintenance(
    pool: &PgPool,
    filter: &MaintenanceFilter,
) -> DbResult<MaintenancePage> {
    #[derive(FromRow)]
    struct Row {
        id: Uuid,
        canonical_title: String,
        content_type: String,
        status: String,
        release_year: Option<i32>,
        providers: Vec<String>,
        source_count: i64,
        chapter_count: i64,
        watcher_count: i64,
        created_at: OffsetDateTime,
        updated_at: OffsetDateTime,
        total: i64,
    }

    let search = filter.search.trim();
    let orphaned = filter.health == SeriesHealth::Orphaned;
    let empty = filter.health == SeriesHealth::Empty;

    // One aggregate pass over `series_sources` rather than a correlated `EXISTS` per predicate:
    // the three filters and the three display columns all want the same per-series rollup, and
    // asking for it six times charges six semi-joins against every row of `series`.
    //
    // `matched` then carries the predicate once, so the page and the total cannot disagree about
    // what "matching" means. Same shape as `repo::user_admin::directory`.
    let rows = sqlx::query_as!(
        Row,
        "WITH carried AS ( \
             SELECT ss.series_id, \
                    count(*) AS sources, \
                    COALESCE(sum(ss.chapter_count), 0)::bigint AS chapters, \
                    array_agg(DISTINCT p.slug) AS slugs \
             FROM series_sources ss JOIN providers p ON p.id = ss.provider_id \
             GROUP BY ss.series_id \
         ), matched AS ( \
             SELECT s.id, s.created_at FROM series s \
             LEFT JOIN carried c ON c.series_id = s.id \
             WHERE ($1 = '' OR s.canonical_title ILIKE '%' || $1 || '%') \
               AND ($2::text IS NULL OR $2 = ANY(c.slugs)) \
               AND (NOT $3::bool OR c.series_id IS NULL) \
               AND (NOT $4::bool OR (c.series_id IS NOT NULL AND c.chapters = 0)) \
         ), page AS ( \
             SELECT id FROM matched ORDER BY created_at DESC, id LIMIT $5 OFFSET $6 \
         ) \
         SELECT s.id, s.canonical_title, \
                s.content_type::text AS \"content_type!\", \
                s.status::text AS \"status!\", \
                s.release_year, s.created_at, s.updated_at, \
                COALESCE(c.slugs, ARRAY[]::text[]) AS \"providers!\", \
                COALESCE(c.sources, 0) AS \"source_count!\", \
                COALESCE(c.chapters, 0) AS \"chapter_count!\", \
                w.count AS \"watcher_count!\", \
                (SELECT count(*) FROM matched) AS \"total!\" \
         FROM page \
         JOIN series s ON s.id = page.id \
         LEFT JOIN carried c ON c.series_id = s.id \
         CROSS JOIN LATERAL ( \
             SELECT count(*) FROM watchlist_entries we WHERE we.series_id = s.id \
         ) AS w(count) \
         ORDER BY s.created_at DESC, s.id",
        search,
        filter.provider_slug.as_deref(),
        orphaned,
        empty,
        filter.limit,
        filter.offset,
    )
    .fetch_all(pool)
    .await?;

    // With no rows there is nothing to read the total from, and it is 0 by definition.
    let total = rows.first().map_or(0, |r| r.total);
    Ok(MaintenancePage {
        total,
        items: rows
            .into_iter()
            .map(|r| MaintenanceRow {
                id: r.id,
                canonical_title: r.canonical_title,
                content_type: r.content_type,
                status: r.status,
                release_year: r.release_year,
                providers: r.providers,
                source_count: r.source_count,
                chapter_count: r.chapter_count,
                watcher_count: r.watcher_count,
                created_at: r.created_at,
                updated_at: r.updated_at,
            })
            .collect(),
    })
}

/// Deployment-wide catalogue totals, for the purge panel's stated blast radius.
///
/// The two junk counts share one rollup, so the whole answer costs one pass over
/// `series_sources` and one join rather than a full `series` scan per count.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — every column is a scalar subquery, so an empty catalogue
/// yields zeros rather than [`crate::DbError::NotFound`].
pub async fn totals<'e, E: PgExecutor<'e>>(exec: E) -> DbResult<CatalogueTotals> {
    let totals = sqlx::query_as!(
        CatalogueTotals,
        "WITH carried AS ( \
             SELECT ss.series_id, COALESCE(sum(ss.chapter_count), 0)::bigint AS chapters \
             FROM series_sources ss GROUP BY ss.series_id \
         ), health AS ( \
             SELECT (c.series_id IS NULL) AS orphaned, \
                    (c.series_id IS NOT NULL AND c.chapters = 0) AS empty \
             FROM series s LEFT JOIN carried c ON c.series_id = s.id \
         ) \
         SELECT \
           (SELECT count(*) FROM health) AS \"series_total!\", \
           (SELECT count(*) FROM series_sources) AS \"sources_total!\", \
           (SELECT count(*) FROM chapters) AS \"chapters_total!\", \
           (SELECT count(*) FROM health WHERE orphaned) AS \"orphaned_series!\", \
           (SELECT count(*) FROM health WHERE empty) AS \"empty_series!\", \
           (SELECT count(*) FROM watchlist_entries) AS \"watchlist_entries!\", \
           (SELECT count(*) FROM read_progress) AS \"progress_rows!\""
    )
    .fetch_one(exec)
    .await?;
    Ok(totals)
}

/// Delete the named series, and everything the schema hangs off them.
///
/// One transaction: a bulk delete that half-applied would leave the operator no way to tell
/// which half. Ids naming nothing are silently skipped — a bulk delete racing a merge must not
/// fail wholesale because one row it listed has already gone.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn delete_series(conn: &mut PgConnection, ids: &[Uuid]) -> DbResult<DeletionReport> {
    if ids.is_empty() {
        return Ok(DeletionReport::default());
    }
    let mut tx = conn.begin().await?;
    let report = measure(&mut tx, ids).await?;
    sqlx::query!("DELETE FROM series WHERE id = ANY($1)", ids)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(report)
}

/// Delete up to `batch` series, oldest first, and report how many are left.
///
/// Batched rather than one `DELETE FROM series`, because a full catalogue cascades into a dozen
/// tables and takes far longer than any HTTP request may: an unbatched purge would hit the
/// request timeout and roll back every time, so the deployment could never actually be emptied.
/// Each call is its own transaction and the operation is resumable — the caller repeats until
/// `remaining` reaches zero.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn purge_series_batch(
    conn: &mut PgConnection,
    batch: i64,
) -> DbResult<(DeletionReport, i64)> {
    let ids: Vec<Uuid> = sqlx::query_scalar!(
        "SELECT id FROM series ORDER BY created_at LIMIT $1",
        batch.max(1)
    )
    .fetch_all(&mut *conn)
    .await?;

    let report = delete_series(conn, &ids).await?;
    let remaining = sqlx::query_scalar!("SELECT count(*) AS \"count!\" FROM series")
        .fetch_one(&mut *conn)
        .await?;
    Ok((report, remaining))
}

/// Delete up to `batch` chapters and report how many are left.
///
/// Series and their sources survive, so the next scan refills them. Batched for the same reason
/// as [`purge_series_batch`].
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn purge_chapters_batch(
    conn: &mut PgConnection,
    batch: i64,
) -> DbResult<(DeletionReport, i64)> {
    let deleted = sqlx::query!(
        // `ctid`, not a key: migration 0055 dropped the surrogate `id`, and the primary key is now
        // the two-column `(series_source_id, number_milli)`, which cannot be fed back through an
        // `IN` as a single scalar. `ctid` is what this always wanted anyway — a physical-order
        // batch, which is the cheapest thing to delete.
        "DELETE FROM chapters WHERE ctid IN (SELECT ctid FROM chapters LIMIT $1)",
        batch.max(1)
    )
    .execute(&mut *conn)
    .await?
    .rows_affected();

    let remaining = sqlx::query_scalar!("SELECT count(*) AS \"count!\" FROM chapters")
        .fetch_one(&mut *conn)
        .await?;
    Ok((
        DeletionReport {
            chapters: i64::try_from(deleted).unwrap_or(i64::MAX),
            ..DeletionReport::default()
        },
        remaining,
    ))
}

/// Count what deleting `ids` will take with it, from inside the deleting transaction.
async fn measure(conn: &mut PgConnection, ids: &[Uuid]) -> DbResult<DeletionReport> {
    let report = sqlx::query_as!(
        DeletionReport,
        "SELECT \
           (SELECT count(*) FROM series s WHERE s.id = ANY($1)) AS \"series!\", \
           (SELECT count(*) FROM series_sources ss WHERE ss.series_id = ANY($1)) \
             AS \"sources!\", \
           (SELECT count(*) FROM chapters c JOIN series_sources ss \
              ON ss.id = c.series_source_id WHERE ss.series_id = ANY($1)) AS \"chapters!\", \
           (SELECT count(*) FROM watchlist_entries we WHERE we.series_id = ANY($1)) \
             AS \"watchlist_entries!\", \
           (SELECT count(*) FROM read_progress rp WHERE rp.series_id = ANY($1)) \
             AS \"progress_rows!\"",
        ids
    )
    .fetch_one(conn)
    .await?;
    Ok(report)
}
