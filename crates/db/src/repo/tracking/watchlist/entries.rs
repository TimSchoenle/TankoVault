//! Membership and status: adding, removing and re-statusing the series a user tracks.

use std::collections::HashMap;

use crate::error::DbResult;
use sqlx::{FromRow, PgExecutor, PgPool};
use tankovault_domain::{SeriesId, SeriesSourceId, UserId, WatchStatus, WatchlistEntry};
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

/// What [`watchlist_set_pinned_source`] did, so the caller can answer with the right status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinOutcome {
    /// The pin was written (or cleared).
    Written,
    /// The caller does not track this series, so there is no entry to pin against.
    NotTracked,
    /// The source exists, but carries a different series.
    ForeignSource,
}

/// Pin the source a series should open on for this reader, or clear the pin with `None`.
///
/// The pin is scoped to the series in SQL, not just validated by a foreign key: `series_sources`
/// ids are global, so without the `EXISTS` a reader could point one series' entry at another
/// series' source and every resolution downstream would follow it.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — a rejected pin is a [`PinOutcome`], not an error.
pub async fn watchlist_set_pinned_source(
    pool: &PgPool,
    user_id: UserId,
    series_id: SeriesId,
    source_id: Option<SeriesSourceId>,
) -> DbResult<PinOutcome> {
    let source = source_id.map(SeriesSourceId::as_uuid);
    let written = sqlx::query!(
        "UPDATE watchlist_entries SET pinned_source_id = $3, updated_at = now() \
         WHERE user_id = $1 AND series_id = $2 \
           AND ($3::uuid IS NULL \
                OR EXISTS (SELECT 1 FROM series_sources ss \
                            WHERE ss.id = $3 AND ss.series_id = $2))",
        user_id.as_uuid(),
        series_id.as_uuid(),
        source,
    )
    .execute(pool)
    .await?
    .rows_affected();
    if written > 0 {
        return Ok(PinOutcome::Written);
    }

    // Nothing moved, and the statement above cannot say which half failed. Ask.
    let tracked = sqlx::query_scalar!(
        "SELECT EXISTS (SELECT 1 FROM watchlist_entries WHERE user_id = $1 AND series_id = $2) \
         AS \"tracked!\"",
        user_id.as_uuid(),
        series_id.as_uuid(),
    )
    .fetch_one(pool)
    .await?;
    Ok(if tracked {
        PinOutcome::ForeignSource
    } else {
        PinOutcome::NotTracked
    })
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

/// The largest number of ids a bulk watchlist operation will act on in one call.
///
/// The cap is enforced at the edge, not here — this constant is what the edge clamps to, so
/// the API and the repo cannot disagree about it. 200 is well past any selection a person
/// makes by hand (select-all over a filtered tab is the realistic maximum) and keeps the
/// `= ANY($2)` array small enough that the statement stays a single index scan.
pub const BULK_ID_LIMIT: usize = 200;

/// Apply a status and/or notify change to many watchlist entries at once, returning the ids
/// that were actually changed.
///
/// **Update, not upsert.** [`watchlist_upsert`] creates the entry it is given; this refuses to,
/// because the bulk bar operates on a selection made *from* the list — an id that is not on it
/// is a stale client, and inserting it would silently re-add a title the user had just removed
/// in another tab. Ids that matched nothing are simply absent from the result, which is what
/// lets the handler answer per-id rather than all-or-nothing.
///
/// `None` for either field leaves that column alone, so "mute 40 titles" does not also
/// normalise their statuses.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. An empty `series_ids`, and a
/// set of ids the user tracks none of, are both an empty `Vec` rather than
/// [`crate::DbError::NotFound`].
pub async fn watchlist_bulk_update<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    series_ids: &[Uuid],
    status: Option<WatchStatus>,
    notify: Option<bool>,
) -> DbResult<Vec<SeriesId>> {
    let changed = sqlx::query_scalar!(
        "UPDATE watchlist_entries \
            SET status = COALESCE($3, status), \
                notify = COALESCE($4, notify), \
                updated_at = now() \
          WHERE user_id = $1 AND series_id = ANY($2) \
          RETURNING series_id",
        user_id.as_uuid(),
        series_ids,
        status as Option<WatchStatus>,
        notify,
    )
    .fetch_all(exec)
    .await?;
    Ok(changed.into_iter().map(SeriesId::from_uuid).collect())
}

/// Remove many watchlist entries at once, returning the ids that were actually removed.
///
/// Unlike [`watchlist_remove`], which is idempotent and cannot tell you whether anything was
/// there, this reports what it deleted: removing 40 titles has to be able to say "38 of these
/// are gone, two were not yours" rather than claim a success it did not have.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. Ids the user was not tracking
/// are absent from the result rather than [`crate::DbError::NotFound`].
pub async fn watchlist_bulk_remove<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    series_ids: &[Uuid],
) -> DbResult<Vec<SeriesId>> {
    let removed = sqlx::query_scalar!(
        "DELETE FROM watchlist_entries \
          WHERE user_id = $1 AND series_id = ANY($2) \
          RETURNING series_id",
        user_id.as_uuid(),
        series_ids,
    )
    .fetch_all(exec)
    .await?;
    Ok(removed.into_iter().map(SeriesId::from_uuid).collect())
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
