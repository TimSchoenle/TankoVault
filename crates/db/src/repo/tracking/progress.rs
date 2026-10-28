//! Read progress: the whole-chapter frontier per series, the mark-read/unread transitions
//! that move it, and the per-series exclusion from external sync (design v2 §A.5).

use std::collections::{HashMap, HashSet};

use crate::error::DbResult;
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::chapter_number::{
    from_milli, milli_ceil, milli_of_whole, whole_ceil, whole_floor,
};
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
    /// Returns whether chapter `number` counts as read (design v2 §A.3).
    ///
    /// A part release belongs to the whole chapter it floors to; hand-rolling `number <= whole`
    /// instead of calling this reports already-read parts as unread forever. SQL read models
    /// mirror this same test inline (see [`super::dashboard::feed`]).
    #[must_use]
    pub fn covers(self, number: f64) -> bool {
        number.floor() <= self.last_read_whole_number
            || (!is_whole(number) && self.last_read_part_number.is_some_and(|p| number <= p))
    }
}

/// Returns whether `number` denotes a whole chapter (integer-valued), not a part release.
#[must_use]
fn is_whole(number: f64) -> bool {
    number.fract() == 0.0
}

/// Sets a user's whole-chapter frontier for a series outright, clearing any now-stale part frontier
/// (design v2 §A.3 / §B.5).
///
/// Serves `PUT /v1/me/progress/{series_id}`, which keeps its "set progress to N" meaning, and
/// external-sync pulls that adopt a remote integer progress.
///
/// Progress on an untracked series is inserted, not refused.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only, including the foreign-key violation of an unknown `series_id`;
/// progress on an untracked series is inserted, not [`crate::DbError::NotFound`].
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

/// Gets both of a user's read frontiers for a series, if tracked (design v2 §A.6 `GET
/// /v1/me/progress/:series_id`).
///
/// `None` means nothing read yet, which callers read as [`ReadProgress::default`](Default).
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; a series with no progress row is `Ok(None)`, which callers read
/// as "nothing read yet" via [`ReadProgress::default`](Default) instead of as
/// [`crate::DbError::NotFound`].
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

/// Gets a user's whole-chapter frontier together with when it last changed, if tracked. Used by
/// external sync to reconcile progress under a `NewestWins` conflict policy (`docs/ARCHITECTURE.md`
/// §B.3).
///
/// The merge keeps `None` distinct from progress of `0` with a timestamp: under `NewestWins` a
/// fabricated timestamp would let a never-read series outrank the remote.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; no progress row is `Ok(None)`, and the merge must keep that
/// distinct from progress of `0` with a timestamp.
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

/// Applies the §A.3 "mark chapter read" rule for a single chapter `number`, advancing whichever
/// frontier is appropriate and never letting a part release corrupt whole-chapter progress.
///
/// A part release ahead of the whole frontier moves *both*: the part frontier to `number`, and
/// the whole frontier up to the last chapter before the one `number` belongs to. Only the first
/// half is "the part cannot corrupt whole-chapter progress"; the second half is what keeps the
/// frontier self-consistent, and it is what external sync sees (§B.5 pushes the whole frontier).
///
/// Marking a whole chapter at or below the whole frontier, or a part of one, leaves both frontiers
/// unchanged. A part the part frontier already covers can still pull the whole frontier up: at
/// whole `1` and part `4.75` with chapter `3` in the catalogue, marking `4.5` moves it to `3`.
/// Every call rewrites the row and its `updated_at`, which [`progress_state`] reads for
/// `NewestWins`. The read and the
/// write are two statements without a transaction: two concurrent marks can interleave, and the
/// loser's advance is lost unreported.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; marking a chapter already covered is `Ok(())` with the
/// frontiers unchanged.
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
        if let Some(p) = part
            && p.floor() <= whole
        {
            part = None; // now stale, superseded by whole-chapter progress
        }
    } else if number.floor() > whole {
        // Ahead of the whole frontier: only the part frontier can record the part release, since
        // `number` is fractional and the whole frontier is integer-valued.
        part = Some(part.map_or(number, |p| p.max(number)));
        // "Mark read" is a *frontier* operation (§A.1: "read through here"), so marking `46.1`
        // also asserts every chapter below 46. Leaving the whole frontier at `40` would report
        // `41`..`45` unread with `46.1` read, and the AniList push, which reads only the whole
        // frontier, would keep sending `40`.
        //
        // The catch-up target comes from the catalogue, as un-reading's does: the highest chapter
        // below `floor(number)`, honouring gaps and part-only chapters (with `5` missing and
        // `4.5`/`4.75` present, marking `6.1` lands on `4`). `floor(number)` itself is excluded:
        // `46.1` ships *ahead of* chapter 46 and says nothing about the rest of it.
        //
        // Skipped when `floor(number) <= whole + 1`, where the query cannot move the frontier, so
        // the sequential case stays at one round trip.
        if number.floor() > whole + 1.0 {
            whole = whole.max(prev_whole_below(pool, series_id, number.floor()).await?);
        }
    }
    // else: already covered by whole-chapter progress; no-op.

    progress_write(pool, user_id, series_id, whole, part).await
}

/// Applies the §A.3 "mark chapter unread" rule for a single chapter `number`, retreating the
/// relevant frontier to just before it.
///
/// Un-reading a whole chapter at or below the whole frontier retreats it to the catalogue's
/// chapter before `number` and clears the part frontier. Un-reading a part of a read whole chapter
/// retreats the whole frontier below that chapter and keeps the parts still read under `number`.
/// Above the whole frontier only a part frontier at or past `number` retreats, to the part release
/// before it; anything else is already unread and nothing moves. Every call rewrites the row, and
/// the read-then-write interleaving of [`progress_mark_read`] applies.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
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
        if number <= whole {
            whole = prev_whole_below(pool, series_id, number).await?;
            part = None;
        } else if part.is_some_and(|p| p >= number) {
            // Ahead of the whole frontier only the part frontier can cover the chapter, so only
            // it retreats. Moving the whole frontier to the chapter before `number` here would
            // mark chapters read that the reader never opened.
            part = prev_part_below(pool, series_id, number, whole).await?;
        }
    } else if number.floor() <= whole {
        // A part of an already-read whole chapter (`152.5` while the frontier is at `152`).
        // Un-reading it necessarily un-reads the chapter that contains it, so the whole
        // frontier retreats below that chapter and the part frontier picks up whatever part
        // is still read underneath. Without this branch the two frontiers cannot express
        // "152.5 unread", and the write would be a silent no-op.
        whole = prev_whole_below(pool, series_id, number.floor()).await?;
        part = prev_part_below(pool, series_id, number, whole).await?;
    } else {
        // Ahead of the whole frontier: the part frontier is the only thing that can move, and
        // it may only move **backwards**. Both halves of that are load-bearing.
        part = match part {
            // Nothing ahead of the whole frontier was read. Retreating from here would *advance*
            // the frontier onto an unread part release, marking another chapter read.
            None => None,
            // The frontier already sits below `number`, so it is unread and nothing moves.
            Some(p) if p < number => Some(p),
            // `number` is at or below the frontier, which retreats to the highest part release
            // the catalogue holds below it. Clearing it would discard every earlier part of the
            // chapter: un-reading `152.6` would report `152.1`..`152.5` unread too.
            Some(_) => prev_part_below(pool, series_id, number, whole).await?,
        };
    }

    progress_write(pool, user_id, series_id, whole, part).await
}

/// Advances many series' progress to "everything read", returning the ids that were on the user's
/// watchlist and therefore acted on. Backs the Watchlist's `Mark group read`.
///
/// # Why the frontier stops at what the reader can open
///
/// The maximum is taken over the chapters this reader may read. Swallowing an unbought paywalled
/// chapter into the frontier would hide it once its timer expires, and the reader would silently
/// lose the chapter they were waiting for.
///
/// # Why the part frontier is cleared
///
/// The whole frontier is set to `max(floor(number))` across every source, and
/// [`ReadProgress::covers`] reports `number` read as soon as `floor(number) <= whole`. Since
/// `max(floor(…))` is by construction at least `floor(x)` for every chapter `x` in the series,
/// that single value covers the part releases too, and a surviving part frontier would be
/// stale, the state the §A.1 invariant forbids.
///
/// # Why `GREATEST`
///
/// The frontier only moves forward, as in `progress_mark_read`. Without it, a catalogue that
/// *lost* chapters (a source delisted, a merge undone) would rewind a reader's progress.
///
/// # Why every watchlisted id comes back
///
/// A tracked series with no chapters has nothing to mark, which is not a failure; reporting it
/// would make "Mark group read" claim a partial failure on a healthy group. The data-modifying
/// `written` CTE runs whether or not it is referenced, and the outer query reads `targets`, so
/// the result is "these ids were yours". Untracked ids are absent.
///
/// One statement, so unlike [`progress_mark_read`] it cannot lose a concurrent advance: the
/// `GREATEST` is evaluated inside the `ON CONFLICT` update against the row it locks.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; ids the user does not track are absent from the result instead of
/// [`crate::DbError::NotFound`].
pub async fn progress_bulk_mark_all_read<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    series_ids: &[Uuid],
) -> DbResult<Vec<SeriesId>> {
    let acted = sqlx::query_scalar!(
        "WITH targets AS ( \
           SELECT w.series_id, latest.n \
           FROM watchlist_entries w \
           CROSS JOIN LATERAL ( \
             SELECT max(c.number_milli / 10000) AS n \
             FROM series_sources ss JOIN chapters c ON c.series_source_id = ss.id \
             WHERE ss.series_id = w.series_id \
               AND (c.access = 'free' OR c.unlocks_at <= now() \
                    OR ss.provider_id = ANY(ARRAY( \
                         SELECT e.provider_id FROM user_provider_early_access e \
                         WHERE e.user_id = $1))) \
           ) latest \
           WHERE w.user_id = $1 AND w.series_id = ANY($2) \
         ), written AS ( \
           INSERT INTO read_progress \
                  (user_id, series_id, last_read_whole_number, last_read_part_number) \
           SELECT $1, t.series_id, t.n, NULL FROM targets t WHERE t.n IS NOT NULL \
           ON CONFLICT (user_id, series_id) DO UPDATE \
              SET last_read_whole_number = GREATEST(read_progress.last_read_whole_number, \
                                                    EXCLUDED.last_read_whole_number), \
                  last_read_part_number  = NULL, \
                  updated_at = now() \
           RETURNING series_id \
         ) \
         SELECT series_id AS \"series_id!\" FROM targets",
        user_id.as_uuid(),
        series_ids,
    )
    .fetch_all(exec)
    .await?;
    Ok(acted.into_iter().map(SeriesId::from_uuid).collect())
}

/// Returns the highest whole chapter that exists for this series strictly below `number`, or `0.0`
/// when there is none (§A.3). Both callers must land on a chapter the catalogue holds, not on
/// `number - 1`: the retreat target for un-reading a whole chapter, and the catch-up target for
/// the whole frontier when a part release ahead of it is marked read.
///
/// "Whole chapter" here means `floor(c.number)`, so a chapter that only ever shipped as parts
/// counts: reading past `46.1` credits chapter `45` even when the catalogue holds only
/// `45.1`..`45.6`, since every part of `45` that exists has been read.
async fn prev_whole_below(pool: &sqlx::PgPool, series_id: SeriesId, number: f64) -> DbResult<f64> {
    // `floor(c.number) < number` in the stored domain. For any real `number`, an integer whole
    // chapter `w` satisfies `w < number` exactly when `w <= ceil(number) - 1`, so the bound on the
    // scaled column is `ceil(number) * 10000` with a strict `<`. Computed here, not in SQL, so
    // the column stays bare on the left of the comparison: an expression over `number_milli`
    // would not match `chapters_source_number_key`.
    let bound = milli_of_whole(whole_ceil(number));
    Ok(sqlx::query_scalar!(
        "SELECT max(c.number_milli / 10000) \
           FROM series_sources ss JOIN chapters c ON c.series_source_id = ss.id \
          WHERE ss.series_id = $1 AND c.number_milli < $2::bigint",
        series_id.as_uuid(),
        bound,
    )
    .fetch_one(pool)
    .await?
    .map_or(0.0, f64::from))
}

/// Returns the highest part release that exists for this series strictly below `number` and
/// belonging to a chapter strictly **ahead** of the whole frontier `whole`, the retreat target for
/// un-reading a part (§A.3).
///
/// The lower bound is `floor(c.number) > whole`, not `c.number > whole`: a part of the whole
/// frontier's own chapter (`152.5` with the frontier at `152`) is already covered, so making it
/// the part frontier records nothing. Every write site produces `floor(part) > whole`.
async fn prev_part_below(
    pool: &sqlx::PgPool,
    series_id: SeriesId,
    number: f64,
    whole: f64,
) -> DbResult<Option<f64>> {
    // Both bounds in the stored domain: `c.number < number` is `number_milli < ceil(number*10000)`
    // (equal for a value that is exact at scale 4, and correct for one that is not), and
    // `floor(c.number) > whole` is `number_milli >= (floor(whole) + 1) * 10000`.
    let upper = milli_ceil(number);
    let lower = milli_of_whole(whole_floor(whole) + 1);
    Ok(sqlx::query_scalar!(
        "SELECT max(c.number_milli) \
           FROM series_sources ss JOIN chapters c ON c.series_source_id = ss.id \
          WHERE ss.series_id = $1 AND c.number_milli < $2::bigint \
            AND c.number_milli >= $3::bigint \
            AND c.number_milli % 10000 <> 0",
        series_id.as_uuid(),
        upper,
        lower,
    )
    .fetch_one(pool)
    .await?
    .map(from_milli))
}

/// Sets (or clears) the blanket per-series sync-exclusion flag on a watchlist entry. The entry must
/// already exist; a series can only be excluded from sync once it is being tracked.
///
/// Returns `true` when the flag was written, `false` when the user tracks no such series.
///
/// The caller decides the status code for `false`: `PUT /v1/me/watchlist/{series_id}/sync`
/// turns it into a 404. This is a privacy setting, so a user opting an untracked series out of
/// sync must not be told the opt-out was saved when nothing was written.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; the precondition above is not enforced *here*.
pub async fn set_sync_excluded<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    series_id: SeriesId,
    excluded: bool,
) -> DbResult<bool> {
    let result = sqlx::query!(
        "UPDATE watchlist_entries SET sync_excluded = $3, updated_at = now() \
         WHERE user_id = $1 AND series_id = $2",
        user_id.as_uuid(),
        series_id.as_uuid(),
        excluded,
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Upserts a per-provider override of the blanket exclusion flag (design v2 §A.5): a specific
/// provider's inclusion or exclusion, taking precedence over `watchlist_entries.sync_excluded`.
///
/// Unlike [`set_sync_excluded`] this writes its own table, so it takes effect whether or not the
/// series is on the watchlist.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; re-setting an existing override updates it instead of raising
/// [`crate::DbError::Conflict`].
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

/// Returns the single choke point every sync path calls before touching a series (design v2 §A.5).
///
/// Precedence: a per-provider override wins outright, even for a series not on the watchlist,
/// then the blanket `sync_excluded` flag, then included.
///
/// Callers **must** propagate the error, never default it to `false`: this choke point honours a
/// user's opt-out, and treating an unavailable database as "included" syncs data they asked to
/// keep out of a third-party service.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; the `COALESCE` chain always yields a row, so every "not
/// configured" case is `Ok(false)`.
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

/// Returns every series `user_id` has excluded from syncing with `provider`, in one query.
///
/// The batched form of [`is_sync_excluded`] for a whole-account reconciliation, with the same
/// precedence: a per-provider override, then the blanket `watchlist_entries.sync_excluded` flag,
/// then included. A series absent from the set is included; an untracked series is in the set
/// only when its override excludes it.
///
/// Propagate the error: an empty set means nothing is excluded, so a swallowed error pushes every
/// series the user excluded.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; the same caution as [`is_sync_excluded`] applies with more force.
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

/// Returns every whole-chapter frontier `user_id` holds, with when it last changed, keyed by
/// series.
///
/// The batched form of [`progress_state`], prefetched once per reconciliation run. Series with
/// no progress row are **absent**, which the merge keeps distinct from progress of `0`, as on
/// [`progress_state`].
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; series with no progress row are **absent** from the map, which
/// the merge must keep distinct from progress of `0` for the reason given on [`progress_state`].
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
        // covers every 152.x, the reason `progress_mark_read` treats marking one a no-op.
        // Disagreeing here would leave that no-op behind a live "mark read" button.
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
