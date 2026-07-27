//! User tracking: watchlist, read progress, and notifications (with fan-out helpers).

use crate::error::DbResult;
use crate::repo::catalog::SeriesListItem;
use serde_json::Value as Json;
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::{
    ContentType, Notification, NotificationId, Series, SeriesId, SeriesStatus, UserId, WatchStatus,
    WatchlistEntry,
};
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

// ---------------------------------------------------------------------------
// Read progress
// ---------------------------------------------------------------------------

/// A user's two independent read frontiers for a series (design v2 §A.1): the highest whole
/// chapter read, plus an optional part-release frontier ahead of it.
#[derive(Debug, Clone, Copy, Default)]
pub struct ReadProgress {
    /// Highest WHOLE chapter number read (integer-valued).
    pub last_read_whole_number: f64,
    /// Highest PART release read ahead of the whole frontier, if any (always fractional).
    pub last_read_part_number: Option<f64>,
}

/// Whether `number` denotes a whole chapter (integer-valued) rather than a part release.
#[must_use]
fn is_whole(number: f64) -> bool {
    number.fract() == 0.0
}

/// Set a user's whole-chapter frontier for a series outright, clearing any now-stale part
/// frontier (design v2 §A.3 / §B.5). Used by the renamed `PUT /v1/me/progress/:series_id`
/// endpoint (which keeps its "set progress to N" semantics) and by external-sync pulls that
/// adopt a remote integer progress.
pub async fn progress_set<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    series_id: SeriesId,
    last_read_whole_number: f64,
) -> DbResult<()> {
    sqlx::query!(
        "INSERT INTO read_progress (user_id, series_id, last_read_whole_number) \
         VALUES ($1,$2,$3::float8::numeric(10,4)) \
         ON CONFLICT (user_id, series_id) DO UPDATE \
            SET last_read_whole_number = EXCLUDED.last_read_whole_number, \
                last_read_part_number = CASE \
                    WHEN read_progress.last_read_part_number IS NOT NULL \
                     AND floor(read_progress.last_read_part_number) <= EXCLUDED.last_read_whole_number \
                    THEN NULL ELSE read_progress.last_read_part_number END, \
                updated_at = now()",
        user_id.as_uuid(),
        series_id.as_uuid(),
        last_read_whole_number,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Low-level write of both frontiers at once. Callers are responsible for upholding the
/// §A.1 invariant (`last_read_part_number IS NULL OR floor(part) >= whole`).
async fn progress_write<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    series_id: SeriesId,
    whole: f64,
    part: Option<f64>,
) -> DbResult<()> {
    sqlx::query!(
        "INSERT INTO read_progress (user_id, series_id, last_read_whole_number, last_read_part_number) \
         VALUES ($1,$2,$3::float8::numeric(10,4),$4::float8::numeric(10,4)) \
         ON CONFLICT (user_id, series_id) DO UPDATE \
            SET last_read_whole_number = EXCLUDED.last_read_whole_number, \
                last_read_part_number  = EXCLUDED.last_read_part_number, \
                updated_at = now()",
        user_id.as_uuid(),
        series_id.as_uuid(),
        whole,
        part,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Get a user's whole-chapter frontier for a series, if tracked.
pub async fn progress_get<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    series_id: SeriesId,
) -> DbResult<Option<f64>> {
    let n = sqlx::query_scalar!(
        "SELECT last_read_whole_number::float8 AS \"last_read_whole_number!\" FROM read_progress \
         WHERE user_id = $1 AND series_id = $2",
        user_id.as_uuid(),
        series_id.as_uuid(),
    )
    .fetch_optional(exec)
    .await?;
    Ok(n)
}

/// Get both of a user's read frontiers for a series, if tracked (design v2 §A.6
/// `GET /v1/me/progress/:series_id`).
pub async fn progress_get_full<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    series_id: SeriesId,
) -> DbResult<Option<ReadProgress>> {
    #[derive(FromRow)]
    struct Row {
        whole: f64,
        part: Option<f64>,
    }
    let row = sqlx::query_as!(
        Row,
        "SELECT last_read_whole_number::float8 AS \"whole!\", \
                last_read_part_number::float8 AS part \
         FROM read_progress WHERE user_id = $1 AND series_id = $2",
        user_id.as_uuid(),
        series_id.as_uuid(),
    )
    .fetch_optional(exec)
    .await?;
    Ok(row.map(|r| ReadProgress {
        last_read_whole_number: r.whole,
        last_read_part_number: r.part,
    }))
}

/// Get a user's whole-chapter frontier together with when it last changed, if tracked. Used
/// by external sync to reconcile progress under a `NewestWins` conflict policy (design §B.3).
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
    let row = sqlx::query_as!(
        Row,
        "SELECT last_read_whole_number::float8 AS \"last!\", updated_at FROM read_progress \
         WHERE user_id = $1 AND series_id = $2",
        user_id.as_uuid(),
        series_id.as_uuid(),
    )
    .fetch_optional(exec)
    .await?;
    Ok(row.map(|r| (r.last, r.updated_at)))
}

/// Apply the §A.3 "mark chapter read" rule for a single chapter `number`, advancing whichever
/// frontier is appropriate and never letting a part release corrupt whole-chapter progress.
pub async fn progress_mark_read(
    pool: &sqlx::PgPool,
    user_id: UserId,
    series_id: SeriesId,
    number: f64,
) -> DbResult<()> {
    let cur = progress_get_full(pool, user_id, series_id)
        .await?
        .unwrap_or_default();
    let mut whole = cur.last_read_whole_number;
    let mut part = cur.last_read_part_number;

    if is_whole(number) {
        whole = whole.max(number);
        if let Some(p) = part {
            if p.floor() <= whole {
                part = None; // now stale, superseded by whole-chapter progress
            }
        }
    } else if number.floor() > whole {
        // Ahead of the whole frontier: advance the part frontier only.
        part = Some(part.map_or(number, |p| p.max(number)));
    }
    // else: already covered by whole-chapter progress; no-op.

    progress_write(pool, user_id, series_id, whole, part).await
}

/// Apply the §A.3 "mark chapter unread" rule for a single chapter `number`, retreating the
/// relevant frontier to just before it. Retreating the whole frontier also clears any part
/// frontier (everything after `number` is un-read).
pub async fn progress_mark_unread(
    pool: &sqlx::PgPool,
    user_id: UserId,
    series_id: SeriesId,
    number: f64,
) -> DbResult<()> {
    let cur = progress_get_full(pool, user_id, series_id)
        .await?
        .unwrap_or_default();
    let mut whole = cur.last_read_whole_number;
    let mut part = cur.last_read_part_number;

    if is_whole(number) {
        // The previous whole chapter that exists for this series strictly below `number`.
        whole = sqlx::query_scalar!(
            "SELECT max(floor(c.number))::float8 \
               FROM series_sources ss JOIN chapters c ON c.series_source_id = ss.id \
              WHERE ss.series_id = $1 AND floor(c.number) < $2::float8",
            series_id.as_uuid(),
            number,
        )
        .fetch_one(pool)
        .await?
        .unwrap_or(0.0);
        part = None;
    } else if part == Some(number) {
        part = None;
    } else {
        // The previous part strictly below `number` that is still ahead of the whole frontier.
        part = sqlx::query_scalar!(
            "SELECT max(c.number)::float8 \
               FROM series_sources ss JOIN chapters c ON c.series_source_id = ss.id \
              WHERE ss.series_id = $1 AND c.number < $2::float8 AND c.number > $3::float8 \
                AND c.number <> floor(c.number)",
            series_id.as_uuid(),
            number,
            whole,
        )
        .fetch_one(pool)
        .await?;
    }

    progress_write(pool, user_id, series_id, whole, part).await
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

// ---------------------------------------------------------------------------
// Per-series external-sync exclusion (design v2 §A.5)
// ---------------------------------------------------------------------------

/// Set (or clear) the blanket per-series sync-exclusion flag on a watchlist entry. The entry
/// must already exist; a series can only be excluded from sync once it is being tracked.
pub async fn set_sync_excluded<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    series_id: SeriesId,
    excluded: bool,
) -> DbResult<()> {
    sqlx::query!(
        "UPDATE watchlist_entries SET sync_excluded = $3, updated_at = now() \
         WHERE user_id = $1 AND series_id = $2",
        user_id.as_uuid(),
        series_id.as_uuid(),
        excluded,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Upsert a per-provider override of the blanket exclusion flag (design v2 §A.5): a specific
/// provider's inclusion/exclusion, taking precedence over `watchlist_entries.sync_excluded`.
pub async fn set_sync_override<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    series_id: SeriesId,
    provider: &str,
    excluded: bool,
) -> DbResult<()> {
    sqlx::query!(
        "INSERT INTO series_sync_overrides (user_id, series_id, provider, excluded) \
         VALUES ($1,$2,$3,$4) \
         ON CONFLICT (user_id, series_id, provider) DO UPDATE SET excluded = EXCLUDED.excluded",
        user_id.as_uuid(),
        series_id.as_uuid(),
        provider,
        excluded,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// The single choke point every sync path calls before touching a series (design v2 §A.5).
/// Precedence: a per-provider override wins outright; otherwise the blanket `sync_excluded`
/// flag; otherwise included. A series not on the watchlist at all is treated as included
/// (there is nothing to exclude yet).
pub async fn is_sync_excluded<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    series_id: SeriesId,
    provider: &str,
) -> DbResult<bool> {
    let excluded = sqlx::query_scalar!(
        "SELECT COALESCE( \
                  (SELECT excluded FROM series_sync_overrides \
                    WHERE user_id = $1 AND series_id = $2 AND provider = $3), \
                  (SELECT sync_excluded FROM watchlist_entries \
                    WHERE user_id = $1 AND series_id = $2), \
                  false) AS \"excluded!\"",
        user_id.as_uuid(),
        series_id.as_uuid(),
        provider,
    )
    .fetch_one(exec)
    .await?;
    Ok(excluded)
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
    sqlx::query!(
        "INSERT INTO notifications (id, user_id, kind, payload) VALUES ($1,$2,$3,$4)",
        id.as_uuid(),
        user_id.as_uuid(),
        kind,
        payload,
    )
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

/// Count a user's unread notifications (used to set the live badge and reconcile on
/// reconnect). Backed by the `notifications_user_unread` partial index.
pub async fn notifications_unread_count<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
) -> DbResult<i64> {
    let count = sqlx::query_scalar!(
        "SELECT count(*) AS \"count!\" FROM notifications WHERE user_id = $1 AND read_at IS NULL",
        user_id.as_uuid(),
    )
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
         WHERE w.user_id = $1 AND NOT ( \
               floor(c.number) <= COALESCE(rp.last_read_whole_number, 0) \
               OR (c.number <> floor(c.number) AND rp.last_read_part_number IS NOT NULL \
                   AND c.number <= rp.last_read_part_number)) \
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

/// Attempt to claim the (user, series, chapter) dedup slot. Returns `true` when this is
/// the first time we are notifying this user for this chapter (row inserted), so the
/// caller should proceed; `false` when already notified (skip).
pub async fn dedup_claim<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    series_id: SeriesId,
    chapter_number: f64,
) -> DbResult<bool> {
    let inserted = sqlx::query!(
        "INSERT INTO notification_dedup (user_id, series_id, chapter_number) \
         VALUES ($1,$2,$3::float8::numeric(10,4)) ON CONFLICT DO NOTHING",
        user_id.as_uuid(),
        series_id.as_uuid(),
        chapter_number,
    )
    .execute(exec)
    .await?
    .rows_affected();
    Ok(inserted == 1)
}

// ---------------------------------------------------------------------------
// Enriched read models for the redesigned Home / Watchlist (frontend §9.3)
// ---------------------------------------------------------------------------

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
                     AND floor(c.number) > COALESCE(rp.last_read_whole_number, 0)) AS \"unread!\" \
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

/// Continue-reading cards: watched series (`reading`/`planned`/`paused`) that have at least
/// one unread chapter, freshest activity first. Returns **every** matching series (no cap) so
/// the rail size is stable across requests; the `unread > 0` requirement is enforced in SQL via
/// `EXISTS` and ties on activity are broken by `series_id` for a deterministic order. `unread`
/// counts distinct **whole** chapters (`floor(number)`) — sub-chapter part releases (e.g.
/// `152.1`..`152.6`) collapse into the one chapter they belong to rather than inflating the badge.
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
                (SELECT min(c.number)::float8 \
                   FROM series_sources ss JOIN chapters c ON c.series_source_id = ss.id \
                   WHERE ss.series_id = w.series_id \
                     AND floor(c.number) > COALESCE(rp.last_read_whole_number, 0)) AS next_number, \
                (SELECT COALESCE(count(DISTINCT floor(c.number)),0) \
                   FROM series_sources ss JOIN chapters c ON c.series_source_id = ss.id \
                   WHERE ss.series_id = w.series_id \
                     AND floor(c.number) > COALESCE(rp.last_read_whole_number, 0)) AS \"unread!\" \
         FROM watchlist_entries w \
         JOIN series s ON s.id = w.series_id \
         LEFT JOIN read_progress rp ON rp.user_id = w.user_id AND rp.series_id = w.series_id \
         WHERE w.user_id = $1 AND w.status IN ('reading','planned','paused') \
           AND EXISTS (SELECT 1 \
                   FROM series_sources ss JOIN chapters c ON c.series_source_id = ss.id \
                   WHERE ss.series_id = w.series_id \
                     AND floor(c.number) > COALESCE(rp.last_read_whole_number, 0)) \
         ORDER BY (SELECT max(c.discovered_at) \
                   FROM series_sources ss JOIN chapters c ON c.series_source_id = ss.id \
                   WHERE ss.series_id = w.series_id) DESC NULLS LAST, w.series_id",
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

/// Lifetime reading stats for the Home/Profile headline (frontend §9.3 `GET /v1/me/stats`).
/// `chapters_read` is the sum of whole chapters below each series' last-read marker — an
/// honest proxy over stored progress (there is no per-chapter read-event log, so a daily
/// "streak" is intentionally omitted rather than fabricated).
#[derive(Debug, Clone, Default, serde::Serialize, FromRow, utoipa::ToSchema)]
pub struct MeStats {
    pub tracking: i64,
    pub reading: i64,
    pub completed: i64,
    pub chapters_read: i64,
    pub unread: i64,
}

/// Compute a user's lifetime tracking stats in a single round trip. Both `chapters_read`
/// and `unread` are floored to whole chapters — sub-chapter part releases (e.g. `152.6`)
/// are not "full chapters" for tracking purposes and don't count as extra ones.
pub async fn me_stats<'e, E: PgExecutor<'e>>(exec: E, user_id: UserId) -> DbResult<MeStats> {
    let stats = sqlx::query_as!(
        MeStats,
        "SELECT \
           (SELECT count(*) FROM watchlist_entries WHERE user_id = $1) AS \"tracking!\", \
           (SELECT count(*) FROM watchlist_entries WHERE user_id = $1 AND status = 'reading') AS \"reading!\", \
           (SELECT count(*) FROM watchlist_entries WHERE user_id = $1 AND status = 'completed') AS \"completed!\", \
           (SELECT COALESCE(sum(floor(last_read_whole_number)),0)::int8 FROM read_progress \
              WHERE user_id = $1) AS \"chapters_read!\", \
           (SELECT count(*) FROM ( \
               SELECT DISTINCT w.series_id, floor(c.number) \
               FROM watchlist_entries w \
               JOIN series_sources ss ON ss.series_id = w.series_id \
               JOIN chapters c ON c.series_source_id = ss.id \
               LEFT JOIN read_progress rp ON rp.user_id = w.user_id AND rp.series_id = w.series_id \
               WHERE w.user_id = $1 AND floor(c.number) > COALESCE(rp.last_read_whole_number, 0) \
           ) q) AS \"unread!\"",
        user_id.as_uuid(),
    )
    .fetch_one(exec)
    .await?;
    Ok(stats)
}

/// "Because you read" recommendations (frontend §9.3, *Stub*): series that share a tag with
/// the user's watchlist and are not already tracked, most shared tags first. Returns an
/// empty vec when the user has no tagged watchlist yet (the API falls back to recent series).
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
