//! Notifications and the notifier's fan-out primitives.
//!
//! `services/notifier` needs exactly this module and nothing else in `tracking` â€” which is
//! the observation ARCH-5 was making about the old single file.

use std::collections::HashMap;

use crate::error::DbResult;
use serde_json::Value as Json;
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::{Notification, NotificationId, SeriesId, UserId};
use time::OffsetDateTime;
use uuid::Uuid;

/// Insert one identical notification for each user in `user_ids`, returning `(user, id)` pairs.
///
/// One statement rather than one per user (PERF-3): a fan-out writes the *same* document to
/// every watcher â€” only `user_id` varies â€” so `kind` and `payload` are bound once and only the
/// user list is unnested. Ids are generated client-side so `RETURNING` can be paired back to
/// its user without a second lookup.
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

/// List a user's notifications, newest first.
pub async fn notifications_list<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    limit: i64,
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
         WHERE user_id = $1 ORDER BY created_at DESC LIMIT $2",
        user_id.as_uuid(),
        limit,
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

/// Unread-notification counts for `user_ids`, as a map. Used to set the live badge without a
/// client round-trip. Backed by the `notifications_user_unread` partial index.
///
/// Grouped rather than one query per user (PERF-3). Users with no unread rows are absent from
/// the map â€” `GROUP BY` cannot invent a zero row â€” so callers must treat a miss as `0`.
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

/// Mark the given notifications read (scoped to the owning user).
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
    pub last_read_number: Option<f64>,
}

/// All users watching `series_id` with `notify = true`, plus their read progress, so
/// the notifier can skip chapters at or below what a user has already read.
pub async fn watchers_for_series<'e, E: PgExecutor<'e>>(
    exec: E,
    series_id: SeriesId,
) -> DbResult<Vec<Watcher>> {
    #[derive(FromRow)]
    struct Row {
        user_id: Uuid,
        last_read_number: Option<f64>,
    }
    let rows = sqlx::query_as!(
        Row,
        "SELECT w.user_id, rp.last_read_whole_number::float8 AS last_read_number \
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
            last_read_number: r.last_read_number,
        })
        .collect())
}

/// Claim the `(user, series, chapter)` dedup slot for every user in `user_ids` at once,
/// returning the subset that was actually claimed â€” i.e. the users this chapter is genuinely
/// new to.
///
/// # Why this is set-based
///
/// The notifier used to call a single-row version of this once per watcher, so announcing one
/// chapter on a series with ten thousand watchers cost ten thousand sequential round trips
/// (PERF-3). `series_id` and `chapter_number` are constant across a fan-out, so only the user
/// list needs unnesting.
///
/// `ON CONFLICT DO NOTHING ... RETURNING` is what makes the claim atomic *and* observable:
/// `RETURNING` yields exactly the rows this statement inserted, so a concurrent notifier
/// handling the same event cannot make both processes believe they claimed the same watcher.
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
