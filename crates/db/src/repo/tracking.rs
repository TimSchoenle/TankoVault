//! User tracking: watchlist, read progress, and notifications (with fan-out helpers).

use crate::error::{DbError, DbResult};
use tankovault_domain::{Notification, NotificationId, SeriesId, UserId, WatchStatus, WatchlistEntry};
use serde_json::Value as Json;
use sqlx::{FromRow, PgExecutor};
use time::OffsetDateTime;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Watchlist
// ---------------------------------------------------------------------------

/// Add or update a watchlist entry.
pub async fn watchlist_upsert<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    series_id: SeriesId,
    status: WatchStatus,
    notify: bool,
) -> DbResult<()> {
    sqlx::query(
        "INSERT INTO watchlist_entries (user_id, series_id, status, notify) \
         VALUES ($1,$2,$3::watch_status,$4) \
         ON CONFLICT (user_id, series_id) DO UPDATE \
            SET status = EXCLUDED.status, notify = EXCLUDED.notify",
    )
    .bind(user_id.as_uuid())
    .bind(series_id.as_uuid())
    .bind(status.as_str())
    .bind(notify)
    .execute(exec)
    .await?;
    Ok(())
}

/// Remove a watchlist entry.
pub async fn watchlist_remove<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    series_id: SeriesId,
) -> DbResult<()> {
    sqlx::query("DELETE FROM watchlist_entries WHERE user_id = $1 AND series_id = $2")
        .bind(user_id.as_uuid())
        .bind(series_id.as_uuid())
        .execute(exec)
        .await?;
    Ok(())
}

/// List a user's watchlist entries.
pub async fn watchlist_list<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
) -> DbResult<Vec<WatchlistEntry>> {
    #[derive(FromRow)]
    struct Row {
        user_id: Uuid,
        series_id: Uuid,
        status: String,
        notify: bool,
        added_at: OffsetDateTime,
    }
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT user_id, series_id, status::text AS status, notify, added_at \
         FROM watchlist_entries WHERE user_id = $1 ORDER BY added_at DESC",
    )
    .bind(user_id.as_uuid())
    .fetch_all(exec)
    .await?;
    rows.into_iter()
        .map(|r| {
            Ok(WatchlistEntry {
                user_id: UserId::from_uuid(r.user_id),
                series_id: SeriesId::from_uuid(r.series_id),
                status: r.status.parse().map_err(DbError::Enum)?,
                notify: r.notify,
                added_at: r.added_at,
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Read progress
// ---------------------------------------------------------------------------

/// Set a user's last-read chapter number for a series.
pub async fn progress_set<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    series_id: SeriesId,
    last_read_number: f64,
) -> DbResult<()> {
    sqlx::query(
        "INSERT INTO read_progress (user_id, series_id, last_read_number) \
         VALUES ($1,$2,$3::numeric(10,4)) \
         ON CONFLICT (user_id, series_id) DO UPDATE \
            SET last_read_number = EXCLUDED.last_read_number, updated_at = now()",
    )
    .bind(user_id.as_uuid())
    .bind(series_id.as_uuid())
    .bind(last_read_number)
    .execute(exec)
    .await?;
    Ok(())
}

/// Get a user's last-read number for a series, if tracked.
pub async fn progress_get<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    series_id: SeriesId,
) -> DbResult<Option<f64>> {
    let n: Option<f64> = sqlx::query_scalar(
        "SELECT last_read_number::float8 FROM read_progress WHERE user_id = $1 AND series_id = $2",
    )
    .bind(user_id.as_uuid())
    .bind(series_id.as_uuid())
    .fetch_optional(exec)
    .await?;
    Ok(n)
}

/// Get a user's last-read number together with when it last changed, if tracked. Used by
/// external sync to reconcile progress under a `NewestWins` conflict policy (design §15).
pub async fn progress_state<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    series_id: SeriesId,
) -> DbResult<Option<(f64, OffsetDateTime)>> {
    #[derive(FromRow)]
    struct Row {
        last: f64,
        updated_at: OffsetDateTime,
    }
    let row: Option<Row> = sqlx::query_as(
        "SELECT last_read_number::float8 AS last, updated_at FROM read_progress \
         WHERE user_id = $1 AND series_id = $2",
    )
    .bind(user_id.as_uuid())
    .bind(series_id.as_uuid())
    .fetch_optional(exec)
    .await?;
    Ok(row.map(|r| (r.last, r.updated_at)))
}

/// Set a watchlist entry's status without disturbing its `notify` flag, inserting the
/// entry (with `notify` defaulted on) if absent. Used by `AniList` pull to import and
/// refresh statuses without clobbering a user's per-title notification choice.
pub async fn watchlist_set_status<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    series_id: SeriesId,
    status: WatchStatus,
) -> DbResult<()> {
    sqlx::query(
        "INSERT INTO watchlist_entries (user_id, series_id, status) \
         VALUES ($1,$2,$3::watch_status) \
         ON CONFLICT (user_id, series_id) DO UPDATE SET status = EXCLUDED.status",
    )
    .bind(user_id.as_uuid())
    .bind(series_id.as_uuid())
    .bind(status.as_str())
    .execute(exec)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Notifications
// ---------------------------------------------------------------------------

/// Insert a notification row.
pub async fn notification_create<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    kind: &str,
    payload: &Json,
) -> DbResult<NotificationId> {
    let id = NotificationId::new();
    sqlx::query("INSERT INTO notifications (id, user_id, kind, payload) VALUES ($1,$2,$3,$4)")
        .bind(id.as_uuid())
        .bind(user_id.as_uuid())
        .bind(kind)
        .bind(payload)
        .execute(exec)
        .await?;
    Ok(id)
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
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT id, user_id, kind, payload, read_at, created_at FROM notifications \
         WHERE user_id = $1 ORDER BY created_at DESC LIMIT $2",
    )
    .bind(user_id.as_uuid())
    .bind(limit)
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

/// Count a user's unread notifications (used to set the live badge and reconcile on
/// reconnect). Backed by the `notifications_user_unread` partial index.
pub async fn notifications_unread_count<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
) -> DbResult<i64> {
    let (count,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM notifications WHERE user_id = $1 AND read_at IS NULL")
            .bind(user_id.as_uuid())
            .fetch_one(exec)
            .await?;
    Ok(count)
}

/// Mark the given notifications read (scoped to the owning user).
pub async fn notifications_mark_read<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    ids: &[Uuid],
) -> DbResult<u64> {
    let result = sqlx::query(
        "UPDATE notifications SET read_at = now() \
         WHERE user_id = $1 AND id = ANY($2) AND read_at IS NULL",
    )
    .bind(user_id.as_uuid())
    .bind(ids)
    .execute(exec)
    .await?;
    Ok(result.rows_affected())
}

// ---------------------------------------------------------------------------
// Notifier fan-out helpers
// ---------------------------------------------------------------------------

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
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT w.user_id, rp.last_read_number::float8 AS last_read_number \
         FROM watchlist_entries w \
         LEFT JOIN read_progress rp ON rp.user_id = w.user_id AND rp.series_id = w.series_id \
         WHERE w.series_id = $1 AND w.notify",
    )
    .bind(series_id.as_uuid())
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
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT s.id AS series_id, s.canonical_title AS series_title, \
                c.number::float8 AS chapter_number, c.title AS chapter_title, \
                p.slug AS provider_slug, p.base_url AS base_url, \
                c.path AS chapter_path, c.discovered_at \
         FROM watchlist_entries w \
         JOIN series s ON s.id = w.series_id \
         JOIN series_sources ss ON ss.series_id = w.series_id \
         JOIN providers p ON p.id = ss.provider_id \
         JOIN chapters c ON c.series_source_id = ss.id \
         LEFT JOIN read_progress rp ON rp.user_id = w.user_id AND rp.series_id = w.series_id \
         WHERE w.user_id = $1 AND c.number > COALESCE(rp.last_read_number, 0) \
         ORDER BY c.discovered_at DESC \
         LIMIT $2",
    )
    .bind(user_id.as_uuid())
    .bind(limit)
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

/// Attempt to claim the (user, series, chapter) dedup slot. Returns `true` when this is
/// the first time we are notifying this user for this chapter (row inserted), so the
/// caller should proceed; `false` when already notified (skip).
pub async fn dedup_claim<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    series_id: SeriesId,
    chapter_number: f64,
) -> DbResult<bool> {
    let inserted = sqlx::query(
        "INSERT INTO notification_dedup (user_id, series_id, chapter_number) \
         VALUES ($1,$2,$3::numeric(10,4)) ON CONFLICT DO NOTHING",
    )
    .bind(user_id.as_uuid())
    .bind(series_id.as_uuid())
    .bind(chapter_number)
    .execute(exec)
    .await?
    .rows_affected();
    Ok(inserted == 1)
}
