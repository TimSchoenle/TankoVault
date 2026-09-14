//! Verification of `chapter_rollup`, the per-source chapter counts the console reads.
//!
//! Triggers keep the rows exact (migration `0059_chapter_rollup`). What they cannot see is a
//! writer that bypasses them, such as a restore or a session with `session_replication_role =
//! replica`; [`verify_batch`] finds those sources by comparing against a live count and rebuilds
//! them through the database's `chapter_rollup_rebuild`, so there is one spelling of a recount.

use crate::error::DbResult;
use sqlx::PgPool;
use uuid::Uuid;

/// What one verification batch found.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RollupCheck {
    /// Sources compared.
    pub checked: u64,
    /// Sources whose stored total disagreed with their chapter count.
    pub total: u64,
    /// Sources whose stored seven-day count disagreed with their chapters of the last seven days.
    pub recent: u64,
    /// The last source compared, to resume after; `None` once the batch reached the end.
    pub resume_after: Option<Uuid>,
}

impl RollupCheck {
    /// Sources that disagreed on either figure.
    #[must_use]
    pub const fn drifted(&self) -> u64 {
        self.total + self.recent
    }
}

/// Compare up to `limit` sources after `after` (in id order) with a live count, rebuild the ones
/// that disagree, and report what was found.
///
/// One statement reads both sides, so a chapter an ingest commits meanwhile is either in both
/// the count and the rollup or in neither, and is never reported as drift.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn verify_batch(pool: &PgPool, after: Option<Uuid>, limit: i64) -> DbResult<RollupCheck> {
    let rows = sqlx::query!(
        "WITH batch AS ( \
             SELECT id FROM series_sources \
             WHERE $1::uuid IS NULL OR id > $1 \
             ORDER BY id LIMIT $2 \
         ) \
         SELECT b.id AS \"id!\", \
                (SELECT count(*) FROM chapters c WHERE c.series_source_id = b.id) \
                  IS DISTINCT FROM \
                (SELECT COALESCE(sum(r.chapters), 0) FROM chapter_rollup r \
                  WHERE r.series_source_id = b.id) AS \"total!\", \
                (SELECT count(*) FROM chapters c WHERE c.series_source_id = b.id \
                   AND c.discovered_at > now() - interval '7 days') \
                  IS DISTINCT FROM \
                (SELECT COALESCE(sum(r.chapters), 0) FROM chapter_rollup r \
                  WHERE r.series_source_id = b.id \
                    AND r.discovered_at > now() - interval '7 days') AS \"recent!\" \
         FROM batch b \
         ORDER BY b.id",
        after,
        limit,
    )
    .fetch_all(pool)
    .await?;

    let mut check = RollupCheck {
        checked: rows.len() as u64,
        resume_after: rows.last().map(|r| r.id),
        ..RollupCheck::default()
    };
    if i64::try_from(rows.len()).unwrap_or(i64::MAX) < limit {
        check.resume_after = None;
    }
    let mut rebuild = Vec::new();
    for row in &rows {
        check.total += u64::from(row.total);
        check.recent += u64::from(row.recent);
        if row.total || row.recent {
            rebuild.push(row.id);
        }
    }
    if !rebuild.is_empty() {
        sqlx::query!("SELECT chapter_rollup_rebuild($1)", &rebuild)
            .fetch_one(pool)
            .await?;
    }
    Ok(check)
}
