//! Notifications and the notifier's fan-out primitives.
//!
//! `services/notifier` needs exactly this module and nothing else in `tracking`.

use std::collections::HashMap;

use super::ReadProgress;
use crate::error::DbResult;
use serde_json::Value as Json;
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::{
    Notification, NotificationId, NotificationPrefs, ProviderId, SeriesId, UserId, WatchStatus,
};
use time::OffsetDateTime;
use uuid::Uuid;

/// A notification row the notifier just wrote, with the document as stored.
pub struct CreatedNotification {
    pub user_id: UserId,
    pub notification_id: NotificationId,
    /// The payload *after* any coalescing merge — what the live push has to carry, since a
    /// pushed `count: 1` for a row that now reads "12 new" is a lie the client cannot detect.
    pub payload: Json,
}

/// The display fields a notification payload snapshots, resolved once per event.
pub struct NotificationContext {
    pub series_title: String,
    pub cover_url: Option<String>,
    /// The provider's base, for resolving a chapter's relative path into an openable URL.
    pub base_url: String,
}

/// Resolve the series and provider a chapter event refers to.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; an unknown series or provider slug is `Ok(None)`, which a
/// caller must treat as "write the notification without the decoration", not as a failure.
pub async fn notification_context<'e, E: PgExecutor<'e>>(
    exec: E,
    series_id: SeriesId,
    provider_slug: &str,
) -> DbResult<Option<NotificationContext>> {
    let row = sqlx::query!(
        "SELECT s.canonical_title, s.cover_url, p.base_url \
         FROM series s JOIN providers p ON p.slug = $2 \
         WHERE s.id = $1",
        series_id.as_uuid(),
        provider_slug,
    )
    .fetch_optional(exec)
    .await?;
    Ok(row.map(|r| NotificationContext {
        series_title: r.canonical_title,
        cover_url: r.cover_url,
        base_url: r.base_url,
    }))
}

/// Insert one identical notification for each user in `user_ids`, in one statement rather than
/// one per user, coalescing into each user's open row when `group_key` is set.
///
/// With a `group_key`, an unread row for the same `(user, group)` absorbs the event instead of
/// adding a second row: `count` sums, `first_number`/`last_number` widen, `latest` keeps whichever
/// side is further along, and everything else takes the newer document's value so a renamed series
/// or a fresh cover lands. `notifications_open_group_idx` is both the conflict target and the
/// concurrency guard — two notifiers handling different chapters of one series serialise on it
/// rather than racing into two rows, so no retry loop is needed here.
///
/// `None` inserts unconditionally: the partial index excludes NULL `group_key`, so ungrouped kinds
/// keep one row per event.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; all-or-nothing — one failure drops the whole fan-out.
pub async fn notifications_upsert_many<'e, E: PgExecutor<'e>>(
    exec: E,
    user_ids: &[UserId],
    kind: &str,
    group_key: Option<&str>,
    payload: &Json,
) -> DbResult<Vec<CreatedNotification>> {
    if user_ids.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<Uuid> = user_ids
        .iter()
        .map(|_| NotificationId::new().as_uuid())
        .collect();
    let users: Vec<Uuid> = user_ids.iter().map(|u| u.as_uuid()).collect();
    let rows = sqlx::query!(
        "INSERT INTO notifications (id, user_id, kind, group_key, payload) \
         SELECT i, u, $3, $4, $5 FROM UNNEST($1::uuid[], $2::uuid[]) AS t(i, u) \
         ON CONFLICT (user_id, group_key) WHERE read_at IS NULL AND group_key IS NOT NULL \
         DO UPDATE SET \
           payload = notifications.payload || EXCLUDED.payload || jsonb_build_object( \
             'count', COALESCE((notifications.payload->>'count')::int, 1) \
                    + COALESCE((EXCLUDED.payload->>'count')::int, 1), \
             'first_number', LEAST((notifications.payload->>'first_number')::float8, \
                                   (EXCLUDED.payload->>'first_number')::float8), \
             'last_number', GREATEST((notifications.payload->>'last_number')::float8, \
                                     (EXCLUDED.payload->>'last_number')::float8), \
             'latest', CASE WHEN COALESCE((EXCLUDED.payload->>'last_number')::float8, 0) \
                              >= COALESCE((notifications.payload->>'last_number')::float8, 0) \
                            THEN EXCLUDED.payload->'latest' \
                            ELSE notifications.payload->'latest' END \
           ), \
           created_at = now() \
         RETURNING id, user_id, payload AS \"payload: Json\"",
        &ids,
        &users,
        kind,
        group_key,
        payload,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| CreatedNotification {
            user_id: UserId::from_uuid(r.user_id),
            notification_id: NotificationId::from_uuid(r.id),
            payload: r.payload,
        })
        .collect())
}

/// Which rows of the inbox a request wants.
///
/// Applied server-side rather than by the client over a loaded page: the tabs used to filter one
/// 50-row batch, so "Unread" showed an empty page whenever the unread rows sat on page two.
#[derive(Debug, Clone, Copy, Default)]
pub struct NotificationFilter<'a> {
    /// Restrict to rows the reader has not read.
    pub unread_only: bool,
    /// Restrict to one `notifications.kind` token.
    pub kind: Option<&'a str>,
}

/// One page of a user's inbox, plus the two totals the page cannot be counted from.
///
/// `total` counts the whole *filtered* inbox and `unread` the whole *unfiltered* one — the pager's
/// denominator and the bell respectively. Deriving either from `items` is what capped both the
/// list and the bell at whatever the page size happened to be.
pub struct NotificationPage {
    /// Newest first.
    pub items: Vec<Notification>,
    /// Notifications matching the filter, in total.
    pub total: i64,
    /// Unread notifications this user has, whatever the filter.
    pub unread: i64,
}

/// One page of a user's notifications, newest first, with the totals the pager and bell need.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; an empty inbox is an empty `Vec` and two zeroes.
pub async fn notifications_page(
    pool: &sqlx::PgPool,
    user_id: UserId,
    filter: NotificationFilter<'_>,
    limit: i64,
    offset: i64,
) -> DbResult<NotificationPage> {
    let (items, totals) = tokio::try_join!(
        notifications_window(pool, user_id, filter, limit, offset),
        notifications_totals(pool, user_id, filter),
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
    filter: NotificationFilter<'_>,
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
         WHERE user_id = $1 \
           AND ($4::bool IS NOT TRUE OR read_at IS NULL) \
           AND ($5::text IS NULL OR kind = $5) \
         ORDER BY created_at DESC LIMIT $2 OFFSET $3",
        user_id.as_uuid(),
        limit,
        offset,
        filter.unread_only,
        filter.kind,
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

/// `(filtered total, inbox-wide unread)`, counted in one pass.
async fn notifications_totals<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    filter: NotificationFilter<'_>,
) -> DbResult<(i64, i64)> {
    let row = sqlx::query!(
        "SELECT count(*) FILTER (WHERE ($2::bool IS NOT TRUE OR read_at IS NULL) \
                                   AND ($3::text IS NULL OR kind = $3)) AS \"total!\", \
                count(*) FILTER (WHERE read_at IS NULL) AS \"unread!\" \
         FROM notifications WHERE user_id = $1",
        user_id.as_uuid(),
        filter.unread_only,
        filter.kind,
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

/// A watcher who opted into notifications for a series, with the two things that decide whether
/// they hear about a chapter: their preferences and their read progress.
pub struct Watcher {
    pub user_id: UserId,
    /// The watchlist status this reader has the series in — the axis their preferences filter on.
    pub status: WatchStatus,
    /// Their decoded preference document; a malformed one decodes to the defaults rather than
    /// failing the fan-out, since a broken preference must not cost the reader the notification.
    pub prefs: NotificationPrefs,
    /// Both read frontiers, or `None` with no progress row. Use
    /// [`ReadProgress::covers`](super::ReadProgress::covers), not a hand-rolled comparison.
    pub progress: Option<ReadProgress>,
}

/// All users watching `series_id` with `notify = true`, plus their watchlist status, decoded
/// preferences and both read frontiers.
///
/// Returns the raw inputs rather than a verdict on purpose: the caller must call
/// [`ReadProgress::covers`](super::ReadProgress::covers) rather than hand-rolling `number <=
/// whole`, which announces an already-read part release as new, and it must apply
/// [`NotificationPrefs`] *after* claiming the dedup slot, not before.
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
        status: WatchStatus,
        prefs: Json,
        whole: Option<f64>,
        part: Option<f64>,
    }
    let rows = sqlx::query_as!(
        Row,
        "SELECT w.user_id, w.status AS \"status: WatchStatus\", \
                u.notification_prefs AS \"prefs: Json\", \
                rp.last_read_whole_number::float8 AS whole, \
                rp.last_read_part_number::float8 AS part \
         FROM watchlist_entries w \
         JOIN users u ON u.id = w.user_id \
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
            status: r.status,
            prefs: serde_json::from_value(r.prefs).unwrap_or_default(),
            // `whole` NULL means no progress row at all; `part` alone being NULL is normal.
            progress: r.whole.map(|whole| ReadProgress {
                last_read_whole_number: whole,
                last_read_part_number: r.part,
            }),
        })
        .collect())
}

/// Which of `user_ids` have opted into this provider's early-access chapters.
///
/// One statement over the whole candidate set rather than an `EXISTS` per watcher: a popular
/// series has thousands of watchers and every one of them would otherwise cost a round trip to
/// answer a question about a table with at most a handful of rows per reader.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; nobody opted in is an empty `Vec`, which is the common case
/// and means "announce this to no one" — never "announce to everyone".
pub async fn early_access_opted_in<'e, E: PgExecutor<'e>>(
    exec: E,
    user_ids: &[UserId],
    provider_id: ProviderId,
) -> DbResult<Vec<UserId>> {
    if user_ids.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<Uuid> = user_ids.iter().map(|u| u.as_uuid()).collect();
    let rows = sqlx::query_scalar!(
        "SELECT user_id FROM user_provider_early_access \
         WHERE provider_id = $1 AND user_id = ANY($2::uuid[])",
        provider_id.as_uuid(),
        &ids,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows.into_iter().map(UserId::from_uuid).collect())
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
