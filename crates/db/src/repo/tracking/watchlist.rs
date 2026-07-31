//! The watchlist: which series a user tracks, at what status, and the enriched card the
//! Watchlist board renders.

use std::collections::HashMap;

use crate::error::DbResult;
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::{SeriesId, UserId, WatchStatus, WatchlistEntry};
use time::OffsetDateTime;
use uuid::Uuid;

/// Add or update a watchlist entry.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. Re-adding a tracked series
/// is an update, not [`crate::DbError::Conflict`]; a `series_id` that does not exist is a
/// foreign-key violation and so a 500 rather than a 404, which is safe only because callers
/// resolve the series first.
pub async fn watchlist_upsert<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    series_id: SeriesId,
    status: WatchStatus,
    notify: bool,
) -> DbResult<()> {
    sqlx::query!(
        "INSERT INTO watchlist_entries (user_id, series_id, status, notify) \
         VALUES ($1,$2,$3,$4) \
         ON CONFLICT (user_id, series_id) DO UPDATE \
            SET status = EXCLUDED.status, notify = EXCLUDED.notify, updated_at = now()",
        user_id.as_uuid(),
        series_id.as_uuid(),
        status as WatchStatus,
        notify,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Remove a watchlist entry.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. Removing something the user
/// was not tracking is `Ok(())`, not [`crate::DbError::NotFound`] — the count is not returned
/// at all, so untracking is idempotent and a caller cannot answer "was it there?".
pub async fn watchlist_remove<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    series_id: SeriesId,
) -> DbResult<()> {
    sqlx::query!(
        "DELETE FROM watchlist_entries WHERE user_id = $1 AND series_id = $2",
        user_id.as_uuid(),
        series_id.as_uuid(),
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// List a user's watchlist entries.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. An empty watchlist is an
/// empty `Vec`.
pub async fn watchlist_list<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
) -> DbResult<Vec<WatchlistEntry>> {
    #[derive(FromRow)]
    struct Row {
        user_id: Uuid,
        series_id: Uuid,
        status: WatchStatus,
        notify: bool,
        added_at: OffsetDateTime,
    }
    let rows = sqlx::query_as!(
        Row,
        "SELECT user_id, series_id, status AS \"status: WatchStatus\", notify, added_at \
         FROM watchlist_entries WHERE user_id = $1 ORDER BY added_at DESC",
        user_id.as_uuid(),
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| WatchlistEntry {
            user_id: UserId::from_uuid(r.user_id),
            series_id: SeriesId::from_uuid(r.series_id),
            status: r.status,
            notify: r.notify,
            added_at: r.added_at,
        })
        .collect())
}

/// Set a watchlist entry's status without disturbing its `notify` flag, inserting the
/// entry (with `notify` defaulted on) if absent. Used by `AniList` pull to import and
/// refresh statuses without clobbering a user's per-title notification choice.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. As with
/// [`watchlist_upsert`], an existing entry is updated rather than raised as
/// [`crate::DbError::Conflict`] — which is what lets a pull run repeatedly without the
/// import deciding it has already happened.
pub async fn watchlist_set_status<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    series_id: SeriesId,
    status: WatchStatus,
) -> DbResult<()> {
    sqlx::query!(
        "INSERT INTO watchlist_entries (user_id, series_id, status) \
         VALUES ($1,$2,$3) \
         ON CONFLICT (user_id, series_id) DO UPDATE \
            SET status = EXCLUDED.status, updated_at = now()",
        user_id.as_uuid(),
        series_id.as_uuid(),
        status as WatchStatus,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// A user's current watch status for a series, if tracked. Used by the targeted single-series
/// sync push (design: immediate targeted push) to read local state without fetching the whole
/// watchlist.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. An untracked series is
/// `Ok(None)`, which the targeted push reads as "nothing local to send" rather than as a
/// failure.
pub async fn watchlist_status_get<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    series_id: SeriesId,
) -> DbResult<Option<WatchStatus>> {
    let status = sqlx::query_scalar!(
        "SELECT status AS \"status: WatchStatus\" FROM watchlist_entries WHERE user_id = $1 AND series_id = $2",
        user_id.as_uuid(),
        series_id.as_uuid(),
    )
    .fetch_optional(exec)
    .await?;
    Ok(status)
}

/// Every watchlist status `user_id` holds, keyed by series.
///
/// The batched form of [`watchlist_status_get`], prefetched once per reconciliation run rather
/// than queried per remote entry (PERF-13).
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. Untracked series are
/// **absent** from the map rather than present with a default, so a lookup miss must mean
/// "not tracked" to the caller and never "status unknown".
pub async fn watchlist_statuses_for_user<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
) -> DbResult<HashMap<SeriesId, WatchStatus>> {
    #[derive(FromRow)]
    struct Row {
        series_id: Uuid,
        status: WatchStatus,
    }
    let rows = sqlx::query_as!(
        Row,
        "SELECT series_id, status AS \"status: WatchStatus\" \
         FROM watchlist_entries WHERE user_id = $1",
        user_id.as_uuid(),
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (SeriesId::from_uuid(r.series_id), r.status))
        .collect())
}

/// A watchlist row enriched with the series title + cover and the user's progress, so the
/// Watchlist board renders each card without an N+1 `series_detail` fetch (frontend §9.3).
#[derive(Debug, Clone)]
pub struct WatchlistCard {
    pub series_id: SeriesId,
    pub series_title: String,
    pub cover_url: Option<String>,
    pub status: WatchStatus,
    pub notify: bool,
    pub added_at: OffsetDateTime,
    pub last_read_number: Option<f64>,
    /// Distinct chapters strictly above the user's progress, across all sources.
    pub unread: i64,
    /// Whether this series is opted out of external sync (design v2 §A.5).
    pub sync_excluded: bool,
}

/// List a user's watchlist with the embedded title/cover/progress each card needs. `unread`
/// counts distinct whole chapters (`floor(number)`) so part releases don't inflate it.
///
/// The filter is the fourth copy of the unread predicate documented on
/// [`dashboard`](super::dashboard); it must stay the negation of
/// [`ReadProgress::covers`](super::ReadProgress::covers), or this badge disagrees with the feed
/// that links to the same chapters.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. A user tracking nothing gets
/// an empty `Vec`; a tracked series with no progress row comes back through the `LEFT JOIN`
/// with `last_read_number: None` and its full chapter count as `unread`, which is a valid card
/// rather than a missing one.
pub async fn watchlist_detailed<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
) -> DbResult<Vec<WatchlistCard>> {
    #[derive(FromRow)]
    struct Row {
        series_id: Uuid,
        series_title: String,
        cover_url: Option<String>,
        status: WatchStatus,
        notify: bool,
        added_at: OffsetDateTime,
        last_read_number: Option<f64>,
        unread: i64,
        sync_excluded: bool,
    }
    let rows = sqlx::query_as!(
        Row,
        "SELECT w.series_id, s.canonical_title AS series_title, s.cover_url, \
                w.status AS \"status: WatchStatus\", w.notify, w.added_at, w.sync_excluded, \
                rp.last_read_whole_number::float8 AS last_read_number, \
                (SELECT COALESCE(count(DISTINCT floor(c.number)),0) \
                   FROM series_sources ss JOIN chapters c ON c.series_source_id = ss.id \
                   WHERE ss.series_id = w.series_id \
                     AND floor(c.number) > COALESCE(rp.last_read_whole_number, 0) \
                     AND NOT (c.number <> floor(c.number) \
                              AND rp.last_read_part_number IS NOT NULL \
                              AND c.number <= rp.last_read_part_number)) AS \"unread!\" \
         FROM watchlist_entries w \
         JOIN series s ON s.id = w.series_id \
         LEFT JOIN read_progress rp ON rp.user_id = w.user_id AND rp.series_id = w.series_id \
         WHERE w.user_id = $1 \
         ORDER BY w.added_at DESC",
        user_id.as_uuid(),
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| WatchlistCard {
            series_id: SeriesId::from_uuid(r.series_id),
            series_title: r.series_title,
            cover_url: r.cover_url,
            status: r.status,
            notify: r.notify,
            added_at: r.added_at,
            last_read_number: r.last_read_number,
            unread: r.unread,
            sync_excluded: r.sync_excluded,
        })
        .collect())
}
