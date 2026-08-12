//! Read models for the Home surfaces: unread feed, continue-reading cards and lifetime stats.
//!
//! Recommendations used to live here as a tag-overlap query that scored the whole catalogue on
//! every request. They are now a model — see `repo::recsys` and `docs/RECOMMENDATIONS.md`.
//!
//! The unread predicate is spelled out 8× (3 here, 5 in [`watchlist`](super::watchlist)) as the
//! negation of [`ReadProgress::covers`](super::ReadProgress::covers), because `sqlx` macros need
//! a string literal and cannot share it via `concat!`; `crates/db/tests/repo_tracking.rs` is what
//! catches a copy drifting:
//!
//! ```text
//! floor(c.number) > COALESCE(rp.last_read_whole_number, 0)
//!   AND NOT (c.number <> floor(c.number)
//!            AND rp.last_read_part_number IS NOT NULL
//!            AND c.number <= rp.last_read_part_number)
//!   AND (c.access = 'free' OR c.unlocks_at <= now()
//!        OR ss.provider_id = ANY(ARRAY(
//!             SELECT e.provider_id FROM user_provider_early_access e
//!             WHERE e.user_id = $1)))
//! ```
//!
//! The third clause is the early-access gate. A chapter a provider has published behind a
//! paywall is stored like any other — it has to be, or the row would be re-discovered and
//! re-dated when the timer expires — but counting it as unread tells a reader they are behind
//! on something they cannot open. It becomes readable in one of two ways: its stated unlock
//! time passes, or the reader has told this provider's row in `user_provider_early_access` that
//! they pay for it.
//!
//! **The opt-in set is read from the bind parameter, never from the outer row.** Written as an
//! `EXISTS` correlated to `w.user_id` it is a subplan the planner charges per chapter row: the
//! per-source scans went from an estimated 4.88 to 114.36 and `continue_reading` as a whole from
//! 14 762 to 177 925 — past `jit_above_cost`, so every request additionally paid to JIT-compile
//! 59 functions it had no use for. `w.user_id` *is* `$1` in all eight statements, and written
//! that way the sublink is uncorrelated: one `InitPlan` per execution, then an array membership
//! test per row. Keeping it a bound array instead would change the signature of every one of
//! these queries and their callers, which is how the predicate's history says the bind numbering
//! gets broken.

use crate::error::DbResult;
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::{SeriesId, UserId};
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
           AND (c.access = 'free' OR c.unlocks_at <= now() \
                OR ss.provider_id = ANY(ARRAY( \
                     SELECT e.provider_id FROM user_provider_early_access e \
                     WHERE e.user_id = $1))) \
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
/// **Two laterals, and they must stay two.** `agg` carries the predicate in its `WHERE`, which
/// is what lets `floor(number) > …` reach `chapters_source_floor_num_access_idx` as an index
/// condition and read only the unread tail. Folding `max(discovered_at)` back in would push the
/// predicate into a `FILTER` and the scan back over every chapter of every watched series —
/// 268 k rows, 280 ms, and an estimated cost high enough to buy 190 ms of JIT nothing needed.
/// `act` keeps that `max` exact by asking one source at a time, so each answer is a one-row
/// backward scan of `chapters_source_disc_access_idx`. Ordering by the newest *unread* chapter
/// would collapse the two into one, and is measurably slower: `discovered_at` is not in the
/// unread index, so that scan cannot stay index-only.
///
/// Both indexes carry `access`/`unlocks_at` as `INCLUDE` payload, and the early-access clause is
/// why: a column this predicate reads that its index does not carry costs a heap fetch per row
/// inspected, which is what `0052_activity_covering_index` was written to undo. A new column in
/// the predicate needs a new payload, not just a new clause.
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
           SELECT min(c.number) AS next_number, \
                  count(DISTINCT floor(c.number)) AS unread \
           FROM series_sources ss JOIN chapters c ON c.series_source_id = ss.id \
           WHERE ss.series_id = w.series_id \
             AND floor(c.number) > COALESCE(rp.last_read_whole_number, 0) \
             AND NOT (c.number <> floor(c.number) \
                      AND rp.last_read_part_number IS NOT NULL \
                      AND c.number <= rp.last_read_part_number) \
             AND (c.access = 'free' OR c.unlocks_at <= now() \
                  OR ss.provider_id = ANY(ARRAY( \
                       SELECT e.provider_id FROM user_provider_early_access e \
                       WHERE e.user_id = $1))) \
         ) agg \
         CROSS JOIN LATERAL ( \
           SELECT max((SELECT max(c2.discovered_at) FROM chapters c2 \
                       WHERE c2.series_source_id = ss2.id \
                         AND (c2.access = 'free' OR c2.unlocks_at <= now() \
                              OR ss2.provider_id = ANY(ARRAY( \
                                   SELECT e.provider_id FROM user_provider_early_access e \
                                   WHERE e.user_id = $1))))) \
                    AS last_activity \
           FROM series_sources ss2 WHERE ss2.series_id = w.series_id \
         ) act \
         WHERE w.user_id = $1 AND w.status IN ('reading','planned','paused') \
           AND agg.unread > 0 \
         ORDER BY act.last_activity DESC NULLS LAST, w.series_id",
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
/// `unread` sums a per-series lateral, the same shape as [`continue_reading`]'s: the predicate
/// sits in the lateral's `WHERE`, so `floor(number) > …` becomes an index condition on
/// `chapters_source_floor_num_access_idx` and the scan reads only unread rows. The global `DISTINCT`
/// this replaced could not — `last_read_whole_number` arrived from a join *above* the chapter
/// scan, so every chapter of every watched series was read and then filtered (319 k rows for 851
/// entries). Summing per series equals that `DISTINCT` only because `watchlist_entries` is keyed
/// on `(user_id, series_id)`: one row per series, so no series is counted twice.
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
           (SELECT COALESCE(sum(agg.unread),0)::int8 \
              FROM watchlist_entries w \
              LEFT JOIN read_progress rp ON rp.user_id = w.user_id AND rp.series_id = w.series_id \
              CROSS JOIN LATERAL ( \
                SELECT count(DISTINCT floor(c.number)) AS unread \
                FROM series_sources ss JOIN chapters c ON c.series_source_id = ss.id \
                WHERE ss.series_id = w.series_id \
                  AND floor(c.number) > COALESCE(rp.last_read_whole_number, 0) \
                  AND NOT (c.number <> floor(c.number) \
                           AND rp.last_read_part_number IS NOT NULL \
                           AND c.number <= rp.last_read_part_number) \
                  AND (c.access = 'free' OR c.unlocks_at <= now() \
                       OR ss.provider_id = ANY(ARRAY( \
                            SELECT e.provider_id FROM user_provider_early_access e \
                            WHERE e.user_id = $1))) \
              ) agg \
              WHERE w.user_id = $1) AS \"unread!\"",
        user_id.as_uuid(),
    )
    .fetch_one(exec)
    .await?;
    Ok(stats)
}
