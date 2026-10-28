//! Live chapter timing between two series: when each first saw the chapters both carry.

use crate::error::DbResult;
use sqlx::PgExecutor;
use tankovault_domain::SeriesId;
use tankovault_domain::matching::LiveTiming;

/// Fewest shared live chapters a source pair needs before its median gap says anything.
const MIN_SHARED_LIVE_CHAPTERS: i64 = 3;

/// Returns how the live chapters of `left` and `right` line up, for the source pair that agrees
/// best.
///
/// A chapter is live on a source when that source discovered it more than a day after the later
/// of the two sources' first discoveries. Before then it is back catalogue, found in bulk by the
/// first scan, and its timing measures when scanning started. Chapters join on their whole
/// number, so `10` and `10.5` both count as chapter 10.
///
/// Only `discovered_at`: provider publication dates are mirror upload dates and say nothing.
///
/// No source pair with enough live chapters is the default [`LiveTiming`], whose `median_gap_hours`
/// is `None`.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn live_timing<'e, E: PgExecutor<'e>>(
    exec: E,
    left: SeriesId,
    right: SeriesId,
) -> DbResult<LiveTiming> {
    let best = sqlx::query!(
        r#"WITH src AS (
               SELECT false AS is_right, id FROM series_sources WHERE series_id = $1
               UNION ALL
               SELECT true, id FROM series_sources WHERE series_id = $2
           ), ch AS (
               SELECT src.is_right, src.id AS source_id,
                      c.number_milli / 10000 AS n, c.discovered_at,
                      min(c.discovered_at) OVER (PARTITION BY src.id) AS first_seen
                 FROM src JOIN chapters c ON c.series_source_id = src.id
           ), pair AS (
               SELECT count(*) AS live_n,
                      percentile_cont(0.5) WITHIN GROUP (
                          ORDER BY abs(extract(epoch FROM l.discovered_at - r.discovered_at))
                                   / 3600) AS gap_h
                 FROM ch l
                 JOIN ch r ON r.is_right AND r.n = l.n
                WHERE NOT l.is_right
                  AND l.discovered_at > greatest(l.first_seen, r.first_seen) + interval '1 day'
                  AND r.discovered_at > greatest(l.first_seen, r.first_seen) + interval '1 day'
                GROUP BY l.source_id, r.source_id
           )
           SELECT live_n AS "live_n!", gap_h AS "gap_h!"
             FROM pair
            WHERE live_n >= $3
            ORDER BY gap_h, live_n DESC
            LIMIT 1"#,
        left.as_uuid(),
        right.as_uuid(),
        MIN_SHARED_LIVE_CHAPTERS,
    )
    .fetch_optional(exec)
    .await?;
    Ok(best.map_or_else(LiveTiming::default, |b| LiveTiming {
        shared_live_chapters: b.live_n,
        median_gap_hours: Some(b.gap_h),
    }))
}
