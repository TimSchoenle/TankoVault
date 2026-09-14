//! Upkeep of `watchlist_unread`, the stored per-(reader, series) unread figures.
//!
//! Triggers keep the rows true on every write (migration `0058_watchlist_unread`). Two things
//! they cannot see live here: a locked chapter becoming readable because its `unlocks_at` passed,
//! which [`sweep_unlocks`] handles, and a writer the triggers do not cover, which
//! [`reconcile_batch`] detects and repairs. Both recompute through the database's
//! `refresh_watchlist_unread`, so there is one spelling of every stored value.

use crate::error::DbResult;
use sqlx::PgPool;
use uuid::Uuid;

/// Recompute up to `limit` stored rows whose unlock deadline has passed, and return how many
/// were due.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; nothing due is `Ok(0)`.
pub async fn sweep_unlocks(pool: &PgPool, limit: i64) -> DbResult<u64> {
    let due = sqlx::query!(
        "SELECT user_id, series_id FROM watchlist_unread \
         WHERE next_unlock_at <= now() \
         ORDER BY next_unlock_at \
         LIMIT $1",
        limit,
    )
    .fetch_all(pool)
    .await?;
    let keys: Vec<(Uuid, Uuid)> = due.into_iter().map(|r| (r.user_id, r.series_id)).collect();
    refresh(pool, &keys).await?;
    Ok(keys.len() as u64)
}

/// What one reconciliation batch found, per stored column.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UnreadDrift {
    /// Rows compared against the live function.
    pub checked: u64,
    /// Watchlist entries that had no stored row at all.
    pub missing: u64,
    /// Rows whose `unread_count` disagreed.
    pub unread_count: u64,
    /// Rows whose `next_unread_milli` disagreed.
    pub next_unread: u64,
    /// Rows whose `total_chapters` or `read_count` disagreed.
    pub totals: u64,
    /// Rows whose `latest_milli` or `latest_readable_at` disagreed.
    pub latest: u64,
    /// Rows whose `next_unlock_at` disagreed.
    pub next_unlock: u64,
}

impl UnreadDrift {
    /// Rows that disagreed on anything, missing ones included.
    #[must_use]
    pub const fn drifted(&self) -> u64 {
        self.missing
            + self.unread_count
            + self.next_unread
            + self.totals
            + self.latest
            + self.next_unlock
    }
}

/// Compare the `limit` least recently verified stored rows with the live computation, repair any
/// that disagree, and create rows for up to `limit` watchlist entries that have none.
///
/// A row whose unlock deadline has already passed is repaired but not counted: it is due for
/// [`sweep_unlocks`], not evidence of a missed write.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn reconcile_batch(pool: &PgPool, limit: i64) -> DbResult<UnreadDrift> {
    let mut drift = UnreadDrift::default();

    let missing = sqlx::query!(
        "SELECT w.user_id, w.series_id FROM watchlist_entries w \
         WHERE NOT EXISTS (SELECT 1 FROM watchlist_unread u \
                           WHERE u.user_id = w.user_id AND u.series_id = w.series_id) \
         LIMIT $1",
        limit,
    )
    .fetch_all(pool)
    .await?;
    let missing: Vec<(Uuid, Uuid)> = missing
        .into_iter()
        .map(|r| (r.user_id, r.series_id))
        .collect();
    drift.missing = missing.len() as u64;
    refresh(pool, &missing).await?;

    let rows = sqlx::query!(
        "WITH batch AS ( \
             SELECT user_id, series_id FROM watchlist_unread ORDER BY verified_at LIMIT $1 \
         ), grouped AS ( \
             SELECT user_id, array_agg(series_id) AS series FROM batch GROUP BY user_id \
         ), live AS ( \
             SELECT l.* FROM grouped g \
             CROSS JOIN LATERAL watchlist_unread_live(g.user_id, g.series) l \
         ) \
         SELECT u.user_id AS \"user_id!\", u.series_id AS \"series_id!\", \
                (u.next_unlock_at IS NOT NULL AND u.next_unlock_at <= now()) AS \"due!\", \
                (u.unread_count IS DISTINCT FROM l.unread_count) AS \"unread_count!\", \
                (u.next_unread_milli IS DISTINCT FROM l.next_unread_milli) AS \"next_unread!\", \
                ((u.total_chapters, u.read_count) \
                  IS DISTINCT FROM (l.total_chapters, l.read_count)) AS \"totals!\", \
                ((u.latest_milli, u.latest_readable_at) \
                  IS DISTINCT FROM (l.latest_milli, l.latest_readable_at)) AS \"latest!\", \
                (u.next_unlock_at IS DISTINCT FROM l.next_unlock_at) AS \"next_unlock!\" \
         FROM batch b \
         JOIN watchlist_unread u ON u.user_id = b.user_id AND u.series_id = b.series_id \
         LEFT JOIN live l ON l.user_id = b.user_id AND l.series_id = b.series_id",
        limit,
    )
    .fetch_all(pool)
    .await?;

    let mut checked = Vec::with_capacity(rows.len());
    let mut repair = Vec::new();
    for row in rows {
        let key = (row.user_id, row.series_id);
        checked.push(key);
        let disagrees =
            row.unread_count || row.next_unread || row.totals || row.latest || row.next_unlock;
        if disagrees {
            repair.push(key);
        }
        if disagrees && !row.due {
            drift.unread_count += u64::from(row.unread_count);
            drift.next_unread += u64::from(row.next_unread);
            drift.totals += u64::from(row.totals);
            drift.latest += u64::from(row.latest);
            drift.next_unlock += u64::from(row.next_unlock);
        }
    }
    drift.checked = checked.len() as u64;
    refresh(pool, &repair).await?;

    let (users, series): (Vec<Uuid>, Vec<Uuid>) = checked.into_iter().unzip();
    sqlx::query!(
        "UPDATE watchlist_unread u SET verified_at = now() \
         FROM UNNEST($1::uuid[], $2::uuid[]) AS k(user_id, series_id) \
         WHERE u.user_id = k.user_id AND u.series_id = k.series_id",
        &users,
        &series,
    )
    .execute(pool)
    .await?;

    Ok(drift)
}

/// Recompute the stored rows for `keys`, one reader at a time in key order.
async fn refresh(pool: &PgPool, keys: &[(Uuid, Uuid)]) -> DbResult<()> {
    let mut keys = keys.to_vec();
    keys.sort_unstable();
    for chunk in keys.chunk_by(|a, b| a.0 == b.0) {
        let series: Vec<Uuid> = chunk.iter().map(|(_, s)| *s).collect();
        sqlx::query!(
            "SELECT refresh_watchlist_unread($1, $2)",
            chunk[0].0,
            &series,
        )
        .fetch_one(pool)
        .await?;
    }
    Ok(())
}
