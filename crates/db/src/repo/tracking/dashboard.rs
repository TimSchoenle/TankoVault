//! Read models for the Home surfaces: unread feed, continue-reading cards, lifetime stats and
//! tag-overlap recommendations (frontend §9.3).
//!
//! The unread predicate is spelled out 4× (3 here, 1 in [`watchlist`](super::watchlist)) as the
//! negation of [`ReadProgress::covers`](super::ReadProgress::covers), because `sqlx` macros need
//! a string literal and cannot share it via `concat!`; `crates/db/tests/repo_tracking.rs` is what
//! catches a copy drifting:
//!
//! ```text
//! floor(c.number) > COALESCE(rp.last_read_whole_number, 0)
//!   AND NOT (c.number <> floor(c.number)
//!            AND rp.last_read_part_number IS NOT NULL
//!            AND c.number <= rp.last_read_part_number)
//! ```

use crate::error::DbResult;
use crate::repo::catalog::SeriesListItem;
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::{ContentType, Series, SeriesId, SeriesStatus, UserId};
use time::OffsetDateTime;
use uuid::Uuid;

/// One unread chapter on a watched series, with the fields needed to resolve its link.
#[derive(Debug, Clone)]
pub struct FeedItem {
    pub series_id: SeriesId,
    pub series_title: String,
    pub chapter_number: f64,
    pub chapter_title: Option<String>,
    pub provider_slug: String,
    pub base_url: String,
    pub chapter_path: String,
    pub discovered_at: OffsetDateTime,
}

/// The user's "new chapters" feed (design §11 `GET /v1/me/feed`): chapters on watched
/// series strictly above the user's read progress, most recently discovered first. Rows
/// are per source; the caller may group across providers.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; empty is never evidence the account is gone.
pub async fn feed<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    limit: i64,
) -> DbResult<Vec<FeedItem>> {
    #[derive(FromRow)]
    struct Row {
        series_id: Uuid,
        series_title: String,
        chapter_number: f64,
        chapter_title: Option<String>,
        provider_slug: String,
        base_url: String,
        chapter_path: String,
        discovered_at: OffsetDateTime,
    }
    let rows = sqlx::query_as!(
        Row,
        "SELECT s.id AS series_id, s.canonical_title AS series_title, \
                c.number::float8 AS \"chapter_number!\", c.title AS chapter_title, \
                p.slug AS provider_slug, p.base_url AS base_url, \
                c.path AS chapter_path, c.discovered_at \
         FROM watchlist_entries w \
         JOIN series s ON s.id = w.series_id \
         JOIN series_sources ss ON ss.series_id = w.series_id \
         JOIN providers p ON p.id = ss.provider_id \
         JOIN chapters c ON c.series_source_id = ss.id \
         LEFT JOIN read_progress rp ON rp.user_id = w.user_id AND rp.series_id = w.series_id \
         WHERE w.user_id = $1 \
           AND floor(c.number) > COALESCE(rp.last_read_whole_number, 0) \
           AND NOT (c.number <> floor(c.number) \
                    AND rp.last_read_part_number IS NOT NULL \
                    AND c.number <= rp.last_read_part_number) \
         ORDER BY c.discovered_at DESC \
         LIMIT $2",
        user_id.as_uuid(),
        limit,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| FeedItem {
            series_id: SeriesId::from_uuid(r.series_id),
            series_title: r.series_title,
            chapter_number: r.chapter_number,
            chapter_title: r.chapter_title,
            provider_slug: r.provider_slug,
            base_url: r.base_url,
            chapter_path: r.chapter_path,
            discovered_at: r.discovered_at,
        })
        .collect())
}

/// A "continue reading" card: a tracked, in-progress series with unread chapters, ordered
/// by the most recent chapter activity (frontend §9.3 `GET /v1/me/continue`).
#[derive(Debug, Clone)]
pub struct ContinueCard {
    pub series_id: SeriesId,
    pub series_title: String,
    pub cover_url: Option<String>,
    pub last_read_number: f64,
    /// The lowest unread chapter number above the user's progress, if any.
    pub next_number: Option<f64>,
    pub unread: i64,
}

/// Continue-reading cards: watched series with at least one unread chapter, freshest activity
/// first (no cap, for a stable rail). `unread` counts distinct whole chapters.
///
/// Both aggregates must use the module's unread predicate — filtering on the whole frontier
/// alone leaves a card that can never be cleared (badge stuck on an already-read part).
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; nothing left is an empty `Vec`.
pub async fn continue_reading<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
) -> DbResult<Vec<ContinueCard>> {
    #[derive(FromRow)]
    struct Row {
        series_id: Uuid,
        series_title: String,
        cover_url: Option<String>,
        last_read_number: f64,
        next_number: Option<f64>,
        unread: i64,
    }
    // One lateral pass per row, not four correlated subqueries — must stay one pass, measured
    // far cheaper at scale. `FILTER`, not `WHERE`: pushing the unread predicate into the
    // lateral's `WHERE` would silently redefine the ordering as "newest unread chapter".
    let rows = sqlx::query_as!(
        Row,
        "SELECT w.series_id, s.canonical_title AS series_title, s.cover_url, \
                COALESCE(rp.last_read_whole_number, 0)::float8 AS \"last_read_number!\", \
                agg.next_number::float8 AS next_number, \
                agg.unread AS \"unread!\" \
         FROM watchlist_entries w \
         JOIN series s ON s.id = w.series_id \
         LEFT JOIN read_progress rp ON rp.user_id = w.user_id AND rp.series_id = w.series_id \
         CROSS JOIN LATERAL ( \
           SELECT min(c.number) FILTER ( \
                    WHERE floor(c.number) > COALESCE(rp.last_read_whole_number, 0) \
                      AND NOT (c.number <> floor(c.number) \
                               AND rp.last_read_part_number IS NOT NULL \
                               AND c.number <= rp.last_read_part_number) \
                  ) AS next_number, \
                  COALESCE(count(DISTINCT floor(c.number)) FILTER ( \
                    WHERE floor(c.number) > COALESCE(rp.last_read_whole_number, 0) \
                      AND NOT (c.number <> floor(c.number) \
                               AND rp.last_read_part_number IS NOT NULL \
                               AND c.number <= rp.last_read_part_number) \
                  ), 0) AS unread, \
                  max(c.discovered_at) AS last_activity \
           FROM series_sources ss JOIN chapters c ON c.series_source_id = ss.id \
           WHERE ss.series_id = w.series_id \
         ) agg \
         WHERE w.user_id = $1 AND w.status IN ('reading','planned','paused') \
           AND agg.unread > 0 \
         ORDER BY agg.last_activity DESC NULLS LAST, w.series_id",
        user_id.as_uuid(),
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| ContinueCard {
            series_id: SeriesId::from_uuid(r.series_id),
            series_title: r.series_title,
            cover_url: r.cover_url,
            last_read_number: r.last_read_number,
            next_number: r.next_number,
            unread: r.unread,
        })
        .collect())
}

/// Lifetime reading stats for the Home/Profile headline. `chapters_read` is a proxy over stored
/// progress — there is no per-chapter read-event log, so a "streak" is omitted rather than faked.
#[derive(Debug, Clone, Default, serde::Serialize, FromRow)]
pub struct MeStats {
    pub tracking: i64,
    pub reading: i64,
    pub completed: i64,
    pub chapters_read: i64,
    pub unread: i64,
}

/// Compute a user's lifetime tracking stats in one round trip; both counts are floored to whole
/// chapters.
///
/// The three watchlist counts share one `count(…) FILTER` pass, held in a CTE that is referenced
/// more than once and therefore materialised — three scalar subqueries over the same rows were
/// three scans of them.
///
/// `unread` stays a global `DISTINCT`, not a per-series lateral like [`continue_reading`]'s —
/// measured slower here despite being faster there; the rewrite is not universally right. It
/// leans on `chapters_source_floor_num_idx`, which carries `number` alongside `floor(number)`
/// precisely so this predicate never leaves the index.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; tracking nothing gets zeros, not [`crate::DbError::NotFound`].
pub async fn me_stats<'e, E: PgExecutor<'e>>(exec: E, user_id: UserId) -> DbResult<MeStats> {
    let stats = sqlx::query_as!(
        MeStats,
        "WITH watched AS ( \
           SELECT count(*) AS tracking, \
                  count(*) FILTER (WHERE status = 'reading') AS reading, \
                  count(*) FILTER (WHERE status = 'completed') AS completed \
           FROM watchlist_entries WHERE user_id = $1 \
         ) \
         SELECT \
           (SELECT tracking FROM watched) AS \"tracking!\", \
           (SELECT reading FROM watched) AS \"reading!\", \
           (SELECT completed FROM watched) AS \"completed!\", \
           (SELECT COALESCE(sum(floor(last_read_whole_number)),0)::int8 FROM read_progress \
              WHERE user_id = $1) AS \"chapters_read!\", \
           (SELECT count(*) FROM ( \
               SELECT DISTINCT w.series_id, floor(c.number) \
               FROM watchlist_entries w \
               JOIN series_sources ss ON ss.series_id = w.series_id \
               JOIN chapters c ON c.series_source_id = ss.id \
               LEFT JOIN read_progress rp ON rp.user_id = w.user_id AND rp.series_id = w.series_id \
               WHERE w.user_id = $1 \
                 AND floor(c.number) > COALESCE(rp.last_read_whole_number, 0) \
                 AND NOT (c.number <> floor(c.number) \
                          AND rp.last_read_part_number IS NOT NULL \
                          AND c.number <= rp.last_read_part_number) \
           ) q) AS \"unread!\"",
        user_id.as_uuid(),
    )
    .fetch_one(exec)
    .await?;
    Ok(stats)
}

/// "Because you read" recommendations: untracked series sharing a tag with the watchlist, most
/// shared tags first. Empty when the watchlist has no tags yet (API falls back to recent series).
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; must not be collapsed into the API's empty-case fallback.
pub async fn recommendations<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    limit: i64,
) -> DbResult<Vec<SeriesListItem>> {
    #[derive(FromRow)]
    struct Row {
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
    let rows = sqlx::query_as!(
        Row,
        "WITH liked_tags AS ( \
            SELECT DISTINCT stg.tag_id \
            FROM series_tags stg \
            JOIN watchlist_entries w ON w.series_id = stg.series_id \
            WHERE w.user_id = $1 \
         ) \
         SELECT s.id, s.canonical_title, s.normalized_title, s.description, s.cover_url, \
                s.content_type AS \"content_type: ContentType\", s.status AS \"status: SeriesStatus\", s.release_year, \
                s.created_at, s.updated_at, \
                (SELECT count(*) FROM series_sources ss WHERE ss.series_id = s.id) AS \"source_count!\" \
         FROM series s \
         WHERE EXISTS (SELECT 1 FROM series_tags stg \
                        WHERE stg.series_id = s.id AND stg.tag_id IN (SELECT tag_id FROM liked_tags)) \
           AND NOT EXISTS (SELECT 1 FROM watchlist_entries w \
                            WHERE w.user_id = $1 AND w.series_id = s.id) \
         ORDER BY (SELECT count(*) FROM series_tags stg \
                    WHERE stg.series_id = s.id \
                      AND stg.tag_id IN (SELECT tag_id FROM liked_tags)) DESC, \
                  s.updated_at DESC \
         LIMIT $2",
        user_id.as_uuid(),
        limit,
    )
    .fetch_all(exec)
    .await?;
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
