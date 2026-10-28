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
    /// The recipient, which is also what the push subject is named after.
    pub user_id: UserId,
    /// The persisted row the push mirrors, so a client can dedup against it.
    pub notification_id: NotificationId,
    /// The payload *after* any coalescing merge, which the live push carries: a pushed
    /// `count: 1` for a row that reads "12 new" is a lie the client cannot detect.
    pub payload: Json,
}

/// The display fields a notification payload snapshots, resolved once per event.
pub struct NotificationContext {
    /// The canonical title at the moment of the event, frozen into the payload.
    pub series_title: String,
    /// Its cover then, `None` when no provider supplied one.
    pub cover_url: Option<String>,
    /// The provider's base, for resolving a chapter's relative path into an openable URL.
    pub base_url: String,
}

/// Resolves the series and provider a chapter event refers to.
///
/// An unknown series or provider slug is `None`, which a caller treats as "write the notification
/// without the decoration".
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
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

/// Inserts one identical notification for each user in `user_ids` in one statement, coalescing
/// into each user's open row when `group_key` is set.
///
/// With a `group_key`, an unread row for the same `(user, group)` absorbs the event instead of
/// adding a second row: `count` sums, `first_number` and `last_number` widen, `latest` keeps
/// whichever side is further along, and everything else takes the newer document's value so a
/// renamed series or a fresh cover lands. `notifications_open_group_idx` is both the conflict
/// target and the concurrency guard: two notifiers handling different chapters of one series
/// serialise on it, so no retry loop is needed.
///
/// `None` inserts unconditionally: the partial index excludes NULL `group_key`, so ungrouped kinds
/// keep one row per event.
///
/// All or nothing: one failure drops the whole fan-out.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
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
/// Applied server-side, not by the client over a loaded page, where "Unread" would show an empty
/// page whenever the unread rows sat on page two.
#[derive(Debug, Clone, Copy, Default)]
pub struct NotificationFilter<'a> {
    /// Restrict to rows the reader has not read.
    pub unread_only: bool,
    /// Restrict to one `notifications.kind` token.
    pub kind: Option<&'a str>,
}

/// One page of a user's inbox, plus the two totals the page cannot be counted from.
///
/// `total` counts the whole *filtered* inbox and `unread` the whole *unfiltered* one: the pager's
/// denominator and the bell. Derived from `items`, both would be capped at the page size.
pub struct NotificationPage {
    /// Newest first.
    pub items: Vec<Notification>,
    /// Notifications matching the filter, in total.
    pub total: i64,
    /// Unread notifications this user has, whatever the filter.
    pub unread: i64,
}

/// Returns one page of a user's notifications, newest first, with the totals the pager and bell
/// need.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
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

/// Returns the `[offset, offset + limit)` window of the inbox, newest first.
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

/// Returns the unread-notification counts for `user_ids`, grouped in one query. Users with no
/// unread rows are absent, so callers treat a miss as `0`.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
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

/// Marks every unread notification this user has read, however many that is.
///
/// Not expressible as [`notifications_mark_read`] with a list of ids: the caller only ever holds
/// the page it loaded, so "mark all read" driven from ids silently leaves the rest unread.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
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

/// Marks the given notifications read (scoped to the owning user).
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
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
    /// Who to notify.
    pub user_id: UserId,
    /// The watchlist status this reader has the series in, the axis their preferences filter on.
    pub status: WatchStatus,
    /// Their decoded preference document. A malformed one decodes to the defaults, since a broken
    /// preference must not cost the reader the notification.
    pub prefs: NotificationPrefs,
    /// Both read frontiers, or `None` with no progress row. Use
    /// [`ReadProgress::covers`](super::ReadProgress::covers), not a hand-rolled comparison.
    pub progress: Option<ReadProgress>,
}

/// Returns all users watching `series_id` with `notify = true`, plus their watchlist status,
/// decoded preferences and both read frontiers.
///
/// Returns the raw inputs, not a verdict: the caller calls
/// [`ReadProgress::covers`](super::ReadProgress::covers), since a hand-rolled `number <= whole`
/// announces an already-read part release as new, and applies [`NotificationPrefs`] *after*
/// claiming the dedup slot.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
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

/// Returns which of `user_ids` have opted into this provider's early-access chapters.
///
/// One statement over the whole candidate set, not an `EXISTS` per watcher: a popular series has
/// thousands of watchers, each costing a round trip. An empty result, the common case, means
/// announce to no one.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
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

/// Claims the `(user, series, chapter)` dedup slot for every user in `user_ids` at once, returning
/// the subset genuinely new to this chapter, in one set-based statement.
///
/// `ON CONFLICT DO NOTHING ... RETURNING` makes the claim atomic and observable: a concurrent
/// notifier handling the same event cannot also claim the same watcher.
///
/// An already-claimed watcher is absent from the result, which makes the fan-out idempotent under
/// redelivery.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
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
