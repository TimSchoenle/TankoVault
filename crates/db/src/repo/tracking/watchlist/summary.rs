//! The watchlist summary: per-status counts and the release-bucket grouping.
//!
//! Every statement here aggregates over the user's whole watchlist, so the shape of the two
//! per-series laterals decides what they cost. The unread predicate sits in the lateral's
//! `WHERE` rather than in a `FILTER` over the series' full chapter list — the same rows, but
//! `floor(number) >` becomes an index cond on `chapters_source_number_key` and the scan stays
//! index-only over the unread tail. `latest_chapter_at` is a scalar `max()` per source for the
//! same reason: that is the form the MIN/MAX index optimisation fires on, over
//! `chapters_source_disc_access_idx`. Together they are what keeps these off the 570 ms shape
//! [`fetch_page`](super::page) documents; both indexes carry the early-access columns as
//! `INCLUDE` payload, which is what keeps either scan index-only.

use crate::error::DbResult;
use sqlx::{FromRow, PgExecutor, PgPool};
use tankovault_domain::{SeriesId, UserId, WatchStatus};

use super::query::{ReleaseBucket, ReleaseGroup, WatchlistCounts, WatchlistFilter};

/// The whole watchlist at a glance: per-status counts and the unread total, under **no**
/// filters at all.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WatchlistSummary {
    /// Entries per status, over the whole library.
    pub counts: WatchlistCounts,
    /// Unread chapters across every tracked series, whatever its status.
    pub unread_total: i64,
}

/// The unfiltered shape of a user's watchlist.
///
/// Distinct from [`WatchlistPage::counts`](super::WatchlistPage::counts), which drops only the `status` arm and keeps the
/// search, recency and source filters: those answer "how many would this tab show *given what
/// I have typed*", while this answers "how big is my library" for surfaces with no filter state
/// of their own — a tab badge, a More sheet, a signed-in header.
///
/// One statement rather than the filtered count with an empty filter: with no free-text arm there
/// is no reason to join `series` at all, and the unread sum rides along in the same scan.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. A user tracking nothing is a
/// zeroed summary, not [`crate::DbError::NotFound`].
pub async fn watchlist_summary<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
) -> DbResult<WatchlistSummary> {
    #[derive(FromRow)]
    struct Row {
        status: WatchStatus,
        n: i64,
        degraded: i64,
        unread: i64,
    }
    let rows = sqlx::query_as!(
        Row,
        "SELECT w.status AS \"status!: WatchStatus\", count(*) AS \"n!\", \
                count(*) FILTER (WHERE src.source_degraded) AS \"degraded!\", \
                COALESCE(sum(ch.unread), 0)::int8 AS \"unread!\" \
         FROM watchlist_entries w \
         LEFT JOIN read_progress rp ON rp.user_id = w.user_id AND rp.series_id = w.series_id \
         CROSS JOIN LATERAL ( \
           SELECT COALESCE(count(DISTINCT c.number_milli / 10000), 0) AS unread \
           FROM series_sources ss JOIN chapters c ON c.series_source_id = ss.id \
           WHERE ss.series_id = w.series_id \
             AND c.number_milli >= (floor(COALESCE(rp.last_read_whole_number, 0))::bigint + 1) * 10000 \
             AND NOT (c.number_milli % 10000 <> 0 \
                      AND rp.last_read_part_number IS NOT NULL \
                      AND c.number_milli <= (rp.last_read_part_number * 10000)::bigint) \
             AND (c.access = 'free' OR c.unlocks_at <= now() \
                  OR ss.provider_id = ANY(ARRAY( \
                       SELECT e.provider_id FROM user_provider_early_access e \
                       WHERE e.user_id = $1))) \
         ) ch \
         CROSS JOIN LATERAL ( \
           SELECT COALESCE((array_agg(ss.state <> 'active' OR p.state <> 'active' \
                                      ORDER BY ss.chapter_count DESC, \
                                               ss.last_scanned_at DESC NULLS LAST, \
                                               p.slug))[1], false) AS source_degraded \
           FROM series_sources ss JOIN providers p ON p.id = ss.provider_id \
           WHERE ss.series_id = w.series_id \
         ) src \
         WHERE w.user_id = $1 \
         GROUP BY w.status",
        user_id.as_uuid(),
    )
    .fetch_all(exec)
    .await?;

    let mut summary = WatchlistSummary::default();
    for row in rows {
        summary.counts.add(row.status, row.n, row.degraded);
        summary.unread_total += row.unread;
    }
    Ok(summary)
}

/// How many entries sit at each status under every filter *but* `status`.
///
/// The predicate list is [`fetch_page`]'s minus the status arm and must stay that way — a tab
/// whose count disagrees with the list it opens is worse than one with no count at all. That
/// includes `pattern`, which arrives pre-escaped from
/// [`search_pattern`](super::query::search_pattern).
pub(super) async fn fetch_counts(
    pool: &PgPool,
    user_id: UserId,
    filter: &WatchlistFilter,
    pattern: Option<&str>,
) -> DbResult<Vec<(WatchStatus, i64, i64)>> {
    #[derive(FromRow)]
    struct Row {
        status: WatchStatus,
        n: i64,
        degraded: i64,
    }
    let rows = sqlx::query_as!(
        Row,
        "SELECT w.status AS \"status!: WatchStatus\", count(*) AS \"n!\", \
                count(*) FILTER (WHERE src.source_degraded) AS \"degraded!\" \
         FROM watchlist_entries w \
         JOIN series s ON s.id = w.series_id \
         LEFT JOIN read_progress rp ON rp.user_id = w.user_id AND rp.series_id = w.series_id \
         CROSS JOIN LATERAL ( \
           SELECT COALESCE(count(DISTINCT c.number_milli / 10000), 0) AS unread \
           FROM series_sources ss JOIN chapters c ON c.series_source_id = ss.id \
           WHERE ss.series_id = w.series_id \
             AND c.number_milli >= (floor(COALESCE(rp.last_read_whole_number, 0))::bigint + 1) * 10000 \
             AND NOT (c.number_milli % 10000 <> 0 \
                      AND rp.last_read_part_number IS NOT NULL \
                      AND c.number_milli <= (rp.last_read_part_number * 10000)::bigint) \
             AND (c.access = 'free' OR c.unlocks_at <= now() \
                  OR ss.provider_id = ANY(ARRAY( \
                       SELECT e.provider_id FROM user_provider_early_access e \
                       WHERE e.user_id = $1))) \
         ) ch \
         CROSS JOIN LATERAL ( \
           SELECT max((SELECT max(c.discovered_at) FROM chapters c \
                       WHERE c.series_source_id = ss.id \
                         AND (c.access = 'free' OR c.unlocks_at <= now() \
                              OR ss.provider_id = ANY(ARRAY( \
                                   SELECT e.provider_id FROM user_provider_early_access e \
                                   WHERE e.user_id = $1))))) \
                    AS latest_chapter_at \
           FROM series_sources ss WHERE ss.series_id = w.series_id \
         ) la \
         CROSS JOIN LATERAL ( \
           SELECT COALESCE((array_agg(ss.state <> 'active' OR p.state <> 'active' \
                                      ORDER BY ss.chapter_count DESC, \
                                               ss.last_scanned_at DESC NULLS LAST, \
                                               p.slug))[1], false) AS source_degraded \
           FROM series_sources ss JOIN providers p ON p.id = ss.provider_id \
           WHERE ss.series_id = w.series_id \
         ) src \
         WHERE w.user_id = $1 \
           AND ($2::text IS NULL \
                OR s.canonical_title ILIKE $2 \
                OR EXISTS (SELECT 1 FROM series_titles st \
                           WHERE st.series_id = w.series_id \
                             AND st.title ILIKE $2) \
                OR EXISTS (SELECT 1 FROM series_tags stg JOIN tags t ON t.id = stg.tag_id \
                           WHERE stg.series_id = w.series_id \
                             AND t.name ILIKE $2) \
                OR EXISTS (SELECT 1 FROM series_authors sa JOIN authors a ON a.id = sa.author_id \
                           WHERE sa.series_id = w.series_id \
                             AND a.name ILIKE $2)) \
           AND (NOT $3::boolean OR ch.unread > 0) \
           AND ($4::timestamptz IS NULL OR la.latest_chapter_at >= $4) \
           AND (NOT $5::boolean OR src.source_degraded) \
           AND ($6::uuid IS NULL OR w.series_id = $6) \
         GROUP BY w.status",
        user_id.as_uuid(),
        pattern,
        filter.unread_only,
        filter.released_since,
        filter.source_issues,
        filter.series_id.map(SeriesId::as_uuid),
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.status, r.n, r.degraded))
        .collect())
}

/// The group-header aggregates, over the *whole* filter — `status` included, since the groups
/// band the list the user is actually looking at.
///
/// The bands are rolling windows off the database clock; see [`ReleaseBucket`] for why they are
/// not calendar days. A row with no chapters has no `latest_chapter_at`, both comparisons are
/// `NULL`, and it falls to the `ELSE` arm — which is what keeps `total` (the sum of the bands)
/// equal to the number of rows the page query can return.
pub(super) async fn fetch_groups(
    pool: &PgPool,
    user_id: UserId,
    filter: &WatchlistFilter,
    pattern: Option<&str>,
) -> DbResult<Vec<ReleaseGroup>> {
    #[derive(FromRow)]
    struct Row {
        bucket: String,
        title_count: i64,
        chapter_count: i64,
    }
    let rows = sqlx::query_as!(
        Row,
        "SELECT CASE \
                  WHEN la.latest_chapter_at >= now() - interval '24 hours' THEN 'today' \
                  WHEN la.latest_chapter_at >= now() - interval '7 days'   THEN 'week' \
                  ELSE 'earlier' \
                END AS \"bucket!\", \
                count(*) AS \"title_count!\", \
                COALESCE(sum(ch.unread), 0)::int8 AS \"chapter_count!\" \
         FROM watchlist_entries w \
         JOIN series s ON s.id = w.series_id \
         LEFT JOIN read_progress rp ON rp.user_id = w.user_id AND rp.series_id = w.series_id \
         CROSS JOIN LATERAL ( \
           SELECT COALESCE(count(DISTINCT c.number_milli / 10000), 0) AS unread \
           FROM series_sources ss JOIN chapters c ON c.series_source_id = ss.id \
           WHERE ss.series_id = w.series_id \
             AND c.number_milli >= (floor(COALESCE(rp.last_read_whole_number, 0))::bigint + 1) * 10000 \
             AND NOT (c.number_milli % 10000 <> 0 \
                      AND rp.last_read_part_number IS NOT NULL \
                      AND c.number_milli <= (rp.last_read_part_number * 10000)::bigint) \
             AND (c.access = 'free' OR c.unlocks_at <= now() \
                  OR ss.provider_id = ANY(ARRAY( \
                       SELECT e.provider_id FROM user_provider_early_access e \
                       WHERE e.user_id = $1))) \
         ) ch \
         CROSS JOIN LATERAL ( \
           SELECT max((SELECT max(c.discovered_at) FROM chapters c \
                       WHERE c.series_source_id = ss.id \
                         AND (c.access = 'free' OR c.unlocks_at <= now() \
                              OR ss.provider_id = ANY(ARRAY( \
                                   SELECT e.provider_id FROM user_provider_early_access e \
                                   WHERE e.user_id = $1))))) \
                    AS latest_chapter_at \
           FROM series_sources ss WHERE ss.series_id = w.series_id \
         ) la \
         CROSS JOIN LATERAL ( \
           SELECT COALESCE((array_agg(ss.state <> 'active' OR p.state <> 'active' \
                                      ORDER BY ss.chapter_count DESC, \
                                               ss.last_scanned_at DESC NULLS LAST, \
                                               p.slug))[1], false) AS source_degraded \
           FROM series_sources ss JOIN providers p ON p.id = ss.provider_id \
           WHERE ss.series_id = w.series_id \
         ) src \
         WHERE w.user_id = $1 \
           AND ($2::watch_status IS NULL OR w.status = $2) \
           AND ($3::text IS NULL \
                OR s.canonical_title ILIKE $3 \
                OR EXISTS (SELECT 1 FROM series_titles st \
                           WHERE st.series_id = w.series_id \
                             AND st.title ILIKE $3) \
                OR EXISTS (SELECT 1 FROM series_tags stg JOIN tags t ON t.id = stg.tag_id \
                           WHERE stg.series_id = w.series_id \
                             AND t.name ILIKE $3) \
                OR EXISTS (SELECT 1 FROM series_authors sa JOIN authors a ON a.id = sa.author_id \
                           WHERE sa.series_id = w.series_id \
                             AND a.name ILIKE $3)) \
           AND (NOT $4::boolean OR ch.unread > 0) \
           AND ($5::timestamptz IS NULL OR la.latest_chapter_at >= $5) \
           AND (NOT $6::boolean OR src.source_degraded) \
           AND ($7::uuid IS NULL OR w.series_id = $7) \
         GROUP BY 1",
        user_id.as_uuid(),
        filter.status as Option<WatchStatus>,
        pattern,
        filter.unread_only,
        filter.released_since,
        filter.source_issues,
        filter.series_id.map(SeriesId::as_uuid),
    )
    .fetch_all(pool)
    .await?;

    let mut groups: Vec<ReleaseGroup> = rows
        .into_iter()
        .map(|r| ReleaseGroup {
            bucket: ReleaseBucket::from_token(&r.bucket),
            title_count: r.title_count,
            chapter_count: r.chapter_count,
        })
        .collect();
    groups.sort_unstable_by_key(|g| g.bucket.rank());
    Ok(groups)
}
