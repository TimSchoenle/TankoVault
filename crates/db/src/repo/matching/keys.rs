//! Rebuilding the normalized title keys the trigram indexes and the duplicate sweep read.

use crate::error::DbResult;
use uuid::Uuid;

/// What a normalized-key rebuild changed.
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
pub struct KeyRebuildReport {
    pub series_scanned: i64,
    pub series_updated: i64,
    pub titles_scanned: i64,
    pub titles_updated: i64,
    /// Alternative titles dropped because the corrected rules collapsed them onto a key the
    /// same series already had.
    pub titles_deduplicated: i64,
}

/// Re-derive every stored normalized key through `normalizer`, which is
/// [`tankovault_domain::normalize_title`].
///
/// # Why this exists as an operator action
///
/// `normalized_title` is a *persisted* key: it is written once, at series creation, and every
/// later match is against the stored value. So a change to the normalization rules — like
/// making an apostrophe join a word instead of splitting one — leaves the whole catalogue on
/// keys derived by the previous rules, and the improvement only reaches rows that happen to be
/// re-scanned. `0023_merge_queue.up.sql` bootstraps the rebuild in SQL, but the SQL there is a
/// twin of the Rust function rather than the function itself; this is the authoritative pass,
/// and it is safe to run repeatedly because it only writes rows whose key actually changed.
///
/// Chunked by id so a 26k-row catalogue does not hold one transaction open across the whole
/// rebuild.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn rebuild_normalized_keys(
    pool: &sqlx::PgPool,
    normalizer: fn(&str) -> String,
) -> DbResult<KeyRebuildReport> {
    const CHUNK: i64 = 500;
    let mut report = KeyRebuildReport::default();

    let mut cursor = Uuid::nil();
    loop {
        let rows = sqlx::query!(
            "SELECT id, canonical_title, normalized_title FROM series \
             WHERE id > $1 ORDER BY id LIMIT $2",
            cursor,
            CHUNK,
        )
        .fetch_all(pool)
        .await?;
        if rows.is_empty() {
            break;
        }
        for row in &rows {
            cursor = row.id;
            report.series_scanned += 1;
            let fresh = normalizer(&row.canonical_title);
            if fresh != row.normalized_title {
                sqlx::query!(
                    "UPDATE series SET normalized_title = $2 WHERE id = $1",
                    row.id,
                    fresh,
                )
                .execute(pool)
                .await?;
                report.series_updated += 1;
            }
        }
    }

    // Alternative titles are keyed by `(series_id, normalized)`, so a rewritten key can collide
    // with a row the same series already holds. That is not an error — the two titles now *are*
    // the same key — so the colliding row is dropped rather than the update failing.
    let mut cursor = (Uuid::nil(), String::new());
    loop {
        let rows = sqlx::query!(
            "SELECT series_id, title, normalized FROM series_titles \
             WHERE (series_id, normalized) > ($1, $2) ORDER BY series_id, normalized LIMIT $3",
            cursor.0,
            cursor.1,
            CHUNK,
        )
        .fetch_all(pool)
        .await?;
        if rows.is_empty() {
            break;
        }
        for row in &rows {
            cursor = (row.series_id, row.normalized.clone());
            report.titles_scanned += 1;
            let fresh = normalizer(&row.title);
            if fresh == row.normalized {
                continue;
            }
            let inserted = sqlx::query!(
                "INSERT INTO series_titles (series_id, title, normalized) VALUES ($1,$2,$3) \
                 ON CONFLICT (series_id, normalized) DO NOTHING",
                row.series_id,
                row.title,
                fresh,
            )
            .execute(pool)
            .await?;
            sqlx::query!(
                "DELETE FROM series_titles WHERE series_id = $1 AND normalized = $2",
                row.series_id,
                row.normalized,
            )
            .execute(pool)
            .await?;
            if inserted.rows_affected() > 0 {
                report.titles_updated += 1;
            } else {
                report.titles_deduplicated += 1;
            }
        }
    }

    Ok(report)
}
