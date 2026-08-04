//! Notifications and the notifier's fan-out primitives.
//!
//! `services/notifier` needs exactly this module and nothing else in `tracking`.

use std::collections::HashMap;

use super::ReadProgress;
use crate::error::DbResult;
use serde_json::Value as Json;
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::{Notification, NotificationId, SeriesId, UserId};
use time::OffsetDateTime;
use uuid::Uuid;

/// Insert one identical notification for each user in `user_ids`, returning `(user, id)` pairs,
/// in one statement rather than one per user.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; all-or-nothing — one failure drops the whole fan-out.
pub async fn notifications_create_many<'e, E: PgExecutor<'e>>(
    exec: E,
    user_ids: &[UserId],
    kind: &str,
    payload: &Json,
) -> DbResult<Vec<(UserId, NotificationId)>> {
    if user_ids.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<Uuid> = user_ids
        .iter()
        .map(|_| NotificationId::new().as_uuid())
        .collect();
    let users: Vec<Uuid> = user_ids.iter().map(|u| u.as_uuid()).collect();
    let rows = sqlx::query!(
        "INSERT INTO notifications (id, user_id, kind, payload) \
         SELECT i, u, $3, $4 FROM UNNEST($1::uuid[], $2::uuid[]) AS t(i, u) \
         RETURNING id, user_id",
        &ids,
        &users,
        kind,
        payload,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            (
                UserId::from_uuid(r.user_id),
                NotificationId::from_uuid(r.id),
            )
        })
        .collect())
}

/// One page of a user's inbox, plus the two totals the page cannot be counted from.
///
/// `total` and `unread` describe the whole inbox, not this window. Deriving them from `items`
/// is what capped both the list and the bell at whatever the page size happened to be.
pub struct NotificationPage {
    /// Newest first.
    pub items: Vec<Notification>,
    /// Notifications this user has, in total.
    pub total: i64,
    /// Of those, how many are unread.
    pub unread: i64,
}

/// One page of a user's notifications, newest first, with the inbox-wide totals.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; an empty inbox is an empty `Vec` and two zeroes.
pub async fn notifications_page(
    pool: &sqlx::PgPool,
    user_id: UserId,
    limit: i64,
    offset: i64,
) -> DbResult<NotificationPage> {
    let (items, totals) = tokio::try_join!(
        notifications_window(pool, user_id, limit, offset),
        notifications_totals(pool, user_id),
    )?;
    let (total, unread) = totals;
    Ok(NotificationPage {
        items,
        total,
        unread,
    })
}

/// The `[offset, offset + limit)` window of the inbox, newest first.
async fn notifications_window<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    limit: i64,
    offset: i64,
) -> DbResult<Vec<Notification>> {
    #[derive(FromRow)]
    struct Row {
        id: Uuid,
        user_id: Uuid,
        kind: String,
        payload: Json,
        read_at: Option<OffsetDateTime>,
        created_at: OffsetDateTime,
    }
    let rows = sqlx::query_as!(
        Row,
        "SELECT id, user_id, kind, payload AS \"payload: Json\", read_at, created_at FROM notifications \
         WHERE user_id = $1 ORDER BY created_at DESC LIMIT $2 OFFSET $3",
        user_id.as_uuid(),
        limit,
        offset,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Notification {
            id: NotificationId::from_uuid(r.id),
            user_id: UserId::from_uuid(r.user_id),
            kind: r.kind,
            payload: r.payload,
            read_at: r.read_at,
            created_at: r.created_at,
        })
        .collect())
}

/// `(total, unread)` for the whole inbox, counted in one pass.
async fn notifications_totals<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
) -> DbResult<(i64, i64)> {
    let row = sqlx::query!(
        "SELECT count(*) AS \"total!\", \
                count(*) FILTER (WHERE read_at IS NULL) AS \"unread!\" \
         FROM notifications WHERE user_id = $1",
        user_id.as_uuid(),
    )
    .fetch_one(exec)
    .await?;
    Ok((row.total, row.unread))
}

/// Unread-notification counts for `user_ids`, grouped in one query. Users with no unread rows
/// are absent from the map — callers must treat a miss as `0`.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; an empty `user_ids` is an empty map.
pub async fn notifications_unread_counts<'e, E: PgExecutor<'e>>(
    exec: E,
    user_ids: &[UserId],
) -> DbResult<HashMap<UserId, i64>> {
    if user_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let ids: Vec<Uuid> = user_ids.iter().map(|u| u.as_uuid()).collect();
    let rows = sqlx::query!(
        "SELECT user_id, count(*) AS \"count!\" FROM notifications \
         WHERE user_id = ANY($1) AND read_at IS NULL \
         GROUP BY user_id",
        &ids,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (UserId::from_uuid(r.user_id), r.count))
        .collect())
}

/// Mark every unread notification this user has read, however many that is.
///
/// Not expressible as [`notifications_mark_read`] with a list of ids: the caller only ever holds
/// the page it loaded, so "mark all read" driven from ids silently leaves the rest unread.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; an already-empty inbox is `0`.
pub async fn notifications_mark_all_read<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
) -> DbResult<u64> {
    let result = sqlx::query!(
        "UPDATE notifications SET read_at = now() WHERE user_id = $1 AND read_at IS NULL",
        user_id.as_uuid(),
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected())
}

/// Mark the given notifications read (scoped to the owning user).
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; ids not owned, unknown, or already read all contribute `0`.
pub async fn notifications_mark_read<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    ids: &[Uuid],
) -> DbResult<u64> {
    let result = sqlx::query!(
        "UPDATE notifications SET read_at = now() \
         WHERE user_id = $1 AND id = ANY($2) AND read_at IS NULL",
        user_id.as_uuid(),
        ids,
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected())
}

/// A watcher who opted into notifications for a series, with their read progress.
pub struct Watcher {
    pub user_id: UserId,
    /// Both read frontiers, or `None` with no progress row. Use
    /// [`ReadProgress::covers`](super::ReadProgress::covers), not a hand-rolled comparison.
    pub progress: Option<ReadProgress>,
}

/// All users watching `series_id` with `notify = true`, plus both read frontiers so the notifier
/// can call [`ReadProgress::covers`](super::ReadProgress::covers) instead of hand-rolling
/// `number <= whole`, which announces an already-read part release as new.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; nobody watching is an empty `Vec` — must not be read as
/// "no watchers" and used to silently drop a retry.
pub async fn watchers_for_series<'e, E: PgExecutor<'e>>(
    exec: E,
    series_id: SeriesId,
) -> DbResult<Vec<Watcher>> {
    #[derive(FromRow)]
    struct Row {
        user_id: Uuid,
        whole: Option<f64>,
        part: Option<f64>,
    }
    let rows = sqlx::query_as!(
        Row,
        "SELECT w.user_id, rp.last_read_whole_number::float8 AS whole, \
                rp.last_read_part_number::float8 AS part \
         FROM watchlist_entries w \
         LEFT JOIN read_progress rp ON rp.user_id = w.user_id AND rp.series_id = w.series_id \
         WHERE w.series_id = $1 AND w.notify",
        series_id.as_uuid(),
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Watcher {
            user_id: UserId::from_uuid(r.user_id),
            // `whole` NULL means no progress row at all; `part` alone being NULL is normal.
            progress: r.whole.map(|whole| ReadProgress {
                last_read_whole_number: whole,
                last_read_part_number: r.part,
            }),
        })
        .collect())
}

/// Claim the `(user, series, chapter)` dedup slot for every user in `user_ids` at once,
/// returning the subset genuinely new to this chapter, in one set-based statement.
///
/// `ON CONFLICT DO NOTHING ... RETURNING` makes the claim atomic and observable: a concurrent
/// notifier handling the same event cannot also claim the same watcher.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; an already-claimed watcher is absent from the result (not
/// [`crate::DbError::Conflict`]) — this is what makes the fan-out idempotent under redelivery.
pub async fn dedup_claim_many<'e, E: PgExecutor<'e>>(
    exec: E,
    user_ids: &[UserId],
    series_id: SeriesId,
    chapter_number: f64,
) -> DbResult<Vec<UserId>> {
    if user_ids.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<Uuid> = user_ids.iter().map(|u| u.as_uuid()).collect();
    let rows = sqlx::query_scalar!(
        "INSERT INTO notification_dedup (user_id, series_id, chapter_number) \
         SELECT u, $2, $3::float8::numeric(10,4) FROM UNNEST($1::uuid[]) AS u \
         ON CONFLICT DO NOTHING \
         RETURNING user_id",
        &ids,
        series_id.as_uuid(),
        chapter_number,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows.into_iter().map(UserId::from_uuid).collect())
}
