//! Read progress: the whole-chapter frontier per series, the mark-read/unread transitions
//! that move it, and the per-series exclusion from external sync (design v2 §A.5).

use std::collections::{HashMap, HashSet};

use crate::error::DbResult;
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::{SeriesId, UserId};
use time::OffsetDateTime;
use uuid::Uuid;

/// A user's two independent read frontiers for a series (design v2 §A.1): the highest whole
/// chapter read, plus an optional part-release frontier ahead of it.
#[derive(Debug, Clone, Copy, Default)]
pub struct ReadProgress {
    /// Highest WHOLE chapter number read (integer-valued).
    pub last_read_whole_number: f64,
    /// Highest PART release read ahead of the whole frontier, if any (always fractional).
    pub last_read_part_number: Option<f64>,
}

impl ReadProgress {
    /// Whether chapter `number` counts as read (design v2 §A.3).
    ///
    /// A part release (`152.5`) belongs *to* the whole chapter it floors to — sources ship
    /// parts ahead of the compiled chapter, they are not chapters that follow it — so reading
    /// whole chapter `152` covers every `152.x`. Only ahead of the whole frontier does the
    /// part frontier decide. Callers that hold a whole frontier but no part frontier must not
    /// hand-roll `number <= whole`: that silently reports every part release as unread while
    /// [`progress_mark_read`] treats marking one a no-op, leaving a dead toggle in the UI.
    ///
    /// Read models that must decide this in SQL mirror the same two clauses inline (see
    /// [`feed`]); this is the definition they mirror.
    #[must_use]
    pub fn covers(self, number: f64) -> bool {
        number.floor() <= self.last_read_whole_number
            || (!is_whole(number) && self.last_read_part_number.is_some_and(|p| number <= p))
    }
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
        whole = prev_whole_below(pool, series_id, number).await?;
        part = None;
    } else if number.floor() <= whole {
        // A part of an already-read whole chapter (`152.5` while the frontier is at `152`).
        // Un-reading it necessarily un-reads the chapter that contains it, so the whole
        // frontier retreats below that chapter and the part frontier picks up whatever part
        // is still read underneath. Without this branch the two frontiers cannot express
        // "152.5 unread", and the write would be a silent no-op.
        whole = prev_whole_below(pool, series_id, number.floor()).await?;
        part = prev_part_below(pool, series_id, number, whole).await?;
    } else if part == Some(number) {
        part = None;
    } else {
        part = prev_part_below(pool, series_id, number, whole).await?;
    }

    progress_write(pool, user_id, series_id, whole, part).await
}

/// The highest whole chapter that exists for this series strictly below `number`, or `0.0`
/// when there is none — the retreat target for un-reading a whole chapter (§A.3).
async fn prev_whole_below(pool: &sqlx::PgPool, series_id: SeriesId, number: f64) -> DbResult<f64> {
    // The bound is cast to `numeric` rather than the column's `floor()` being cast to
    // `float8`. Written the other way round, Postgres compares `(floor(number))::float8`,
    // which is a *different* expression from the one `chapters_source_floor_idx
    // (series_source_id, (floor(number)))` indexes, so the index can never match it.
    Ok(sqlx::query_scalar!(
        "SELECT max(floor(c.number))::float8 \
           FROM series_sources ss JOIN chapters c ON c.series_source_id = ss.id \
          WHERE ss.series_id = $1 AND floor(c.number) < ($2::float8)::numeric",
        series_id.as_uuid(),
        number,
    )
    .fetch_one(pool)
    .await?
    .unwrap_or(0.0))
}

/// The highest part release that exists for this series strictly below `number` and still
/// ahead of the whole frontier `whole` — the retreat target for un-reading a part (§A.3).
async fn prev_part_below(
    pool: &sqlx::PgPool,
    series_id: SeriesId,
    number: f64,
    whole: f64,
) -> DbResult<Option<f64>> {
    Ok(sqlx::query_scalar!(
        "SELECT max(c.number)::float8 \
           FROM series_sources ss JOIN chapters c ON c.series_source_id = ss.id \
          WHERE ss.series_id = $1 AND c.number < $2::float8 AND c.number > $3::float8 \
            AND c.number <> floor(c.number)",
        series_id.as_uuid(),
        number,
        whole,
    )
    .fetch_one(pool)
    .await?)
}

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

/// Every series `user_id` has excluded from syncing with `provider`, in one query.
///
/// The whole-account reconciliation used to call [`is_sync_excluded`] per series (PERF-13); the
/// tables are keyed on `user_id`, so one pass over both is enough for a run. The precedence is
/// reproduced exactly: a per-provider override wins outright, otherwise the blanket
/// `watchlist_entries.sync_excluded` flag, otherwise included.
///
/// A series absent from the returned set is included — which is also the answer for a series
/// not on the watchlist at all, matching [`is_sync_excluded`]'s `COALESCE(..., false)` tail.
pub async fn sync_excluded_series<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    provider: &str,
) -> DbResult<HashSet<SeriesId>> {
    let rows = sqlx::query_scalar!(
        "SELECT series_id AS \"series_id!\" FROM ( \
           SELECT w.series_id, \
                  COALESCE(o.excluded, w.sync_excluded) AS excluded \
           FROM watchlist_entries w \
           LEFT JOIN series_sync_overrides o \
             ON o.user_id = w.user_id AND o.series_id = w.series_id AND o.provider = $2 \
           WHERE w.user_id = $1 \
           UNION ALL \
           SELECT o.series_id, o.excluded \
           FROM series_sync_overrides o \
           WHERE o.user_id = $1 AND o.provider = $2 \
             AND NOT EXISTS (SELECT 1 FROM watchlist_entries w \
                             WHERE w.user_id = o.user_id AND w.series_id = o.series_id) \
         ) resolved \
         WHERE excluded",
        user_id.as_uuid(),
        provider,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows.into_iter().map(SeriesId::from_uuid).collect())
}

/// Every whole-chapter frontier `user_id` holds, with when it last changed, keyed by series.
///
/// The batched form of [`progress_state`], prefetched once per reconciliation run rather than
/// queried per remote entry (PERF-13).
pub async fn progress_states_for_user<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
) -> DbResult<HashMap<SeriesId, (f64, OffsetDateTime)>> {
    #[derive(FromRow)]
    struct Row {
        series_id: Uuid,
        last: f64,
        updated_at: OffsetDateTime,
    }
    let rows = sqlx::query_as!(
        Row,
        "SELECT series_id, last_read_whole_number::float8 AS \"last!\", updated_at \
         FROM read_progress WHERE user_id = $1",
        user_id.as_uuid(),
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (SeriesId::from_uuid(r.series_id), (r.last, r.updated_at)))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::ReadProgress;

    fn progress(whole: f64, part: Option<f64>) -> ReadProgress {
        ReadProgress {
            last_read_whole_number: whole,
            last_read_part_number: part,
        }
    }

    #[test]
    fn whole_chapters_are_read_up_to_the_whole_frontier() {
        let p = progress(152.0, None);
        assert!(p.covers(151.0));
        assert!(p.covers(152.0));
        assert!(!p.covers(153.0));
    }

    #[test]
    fn a_part_read_ahead_of_the_whole_frontier_is_read() {
        // The case the two-scalar model exists for: 152.5 read while chapter 152 itself is
        // not out yet. Deciding this from the whole frontier alone reports it unread forever.
        let p = progress(151.0, Some(152.5));
        assert!(p.covers(152.4));
        assert!(p.covers(152.5));
        assert!(!p.covers(152.6));
        assert!(!p.covers(152.0));
    }

    #[test]
    fn parts_of_an_already_read_whole_chapter_are_read() {
        // Parts are fragments shipped ahead of the compiled chapter, so reading whole 152
        // covers every 152.x — the same reason `progress_mark_read` treats marking one a
        // no-op. Disagreeing here would leave that no-op behind a live "mark read" button.
        let p = progress(152.0, None);
        assert!(p.covers(152.1));
        assert!(p.covers(152.9));
        assert!(!p.covers(153.1));
    }

    #[test]
    fn a_zero_frontier_covers_only_chapter_zero_and_its_parts() {
        // `0` is this schema's "nothing read" sentinel *and* a legitimate chapter number, so
        // a zero frontier reads as "chapter 0 done". Every SQL read model resolves the
        // ambiguity the same way (`floor(c.number) > COALESCE(last_read_whole_number, 0)`);
        // callers that must distinguish "no row" from "frontier 0" check the `Option` from
        // `progress_get_full` instead of asking this method.
        let p = progress(0.0, None);
        assert!(p.covers(0.0));
        assert!(p.covers(0.5));
        assert!(!p.covers(1.0));
    }
}
