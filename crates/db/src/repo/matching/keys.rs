//! Rebuilding the normalized title keys the trigram indexes and the duplicate sweep read.

use crate::error::DbResult;
use uuid::Uuid;

/// What a normalized-key rebuild changed.
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
pub struct KeyRebuildReport {
    /// Canonical titles read.
    pub series_scanned: i64,
    /// Of those, whose key the corrected rules changed.
    pub series_updated: i64,
    /// Alternative titles read.
    pub titles_scanned: i64,
    /// Of those, whose key the corrected rules changed.
    pub titles_updated: i64,
    /// Alternative titles dropped because the corrected rules collapsed them onto a key the
    /// same series already had.
    pub titles_deduplicated: i64,
    /// Sources with a listing title read.
    pub sources_scanned: i64,
    /// Of those, whose listing key was missing or changed.
    pub sources_updated: i64,
}

/// Re-derives every stored normalized key through `normalizer`, which is
/// [`tankovault_domain::normalize_title`].
///
/// `normalized_title` is written once, at series creation, and every later match reads the
/// stored value, so a change to the normalization rules reaches only rescanned rows until this
/// runs. `0023_merge_queue.up.sql` bootstraps the rebuild with a SQL twin of the function; this
/// is the authoritative pass, safe to repeat because it writes only rows whose key changed.
/// Chunked by id, so no transaction spans the whole catalogue.
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
    // with a row the same series holds. The two titles are then the same key, so the colliding
    // row is dropped instead of failing the update.
    let mut cursor = (Uuid::nil(), String::new());
    loop {
        let rows = sqlx::query!(
            "SELECT series_id, title, normalized, origin FROM series_titles \
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
            // The origin is carried over: a cleanup targets titles by provenance.
            let inserted = sqlx::query!(
                "INSERT INTO series_titles (series_id, title, normalized, origin) \
                 VALUES ($1,$2,$3,$4) ON CONFLICT (series_id, normalized) DO NOTHING",
                row.series_id,
                row.title,
                fresh,
                row.origin,
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

    rebuild_listing_keys(pool, normalizer, &mut report).await?;
    Ok(report)
}

/// Runs the listing-key pass of [`rebuild_normalized_keys`], which also backfills a source with
/// no key, so a missing key counts as a changed one.
async fn rebuild_listing_keys(
    pool: &sqlx::PgPool,
    normalizer: fn(&str) -> String,
    report: &mut KeyRebuildReport,
) -> DbResult<()> {
    const CHUNK: i64 = 500;
    let mut cursor = Uuid::nil();
    loop {
        let rows = sqlx::query!(
            "SELECT id, provider_title AS \"provider_title!\", provider_title_normalized \
               FROM series_sources \
              WHERE id > $1 AND provider_title IS NOT NULL ORDER BY id LIMIT $2",
            cursor,
            CHUNK,
        )
        .fetch_all(pool)
        .await?;
        if rows.is_empty() {
            break;
        }
        // One statement per chunk: on the first run after migration 0067 every source needs a
        // key, which row by row is a round trip per source.
        let mut ids = Vec::new();
        let mut keys = Vec::new();
        let mut titles = Vec::new();
        for row in &rows {
            cursor = row.id;
            report.sources_scanned += 1;
            let fresh = normalizer(&row.provider_title);
            if row.provider_title_normalized.as_deref() != Some(fresh.as_str()) {
                ids.push(row.id);
                keys.push(fresh);
                titles.push(row.provider_title.clone());
            }
        }
        if !ids.is_empty() {
            // Matched on the title read above: a scan that renamed the source since then wrote
            // the key for its new title, which this one must not overwrite.
            let updated = sqlx::query!(
                "UPDATE series_sources ss SET provider_title_normalized = u.key \
                   FROM UNNEST($1::uuid[], $2::text[], $3::text[]) AS u(id, key, title) \
                  WHERE ss.id = u.id AND ss.provider_title = u.title",
                &ids,
                &keys,
                &titles,
            )
            .execute(pool)
            .await?;
            report.sources_updated += i64::try_from(updated.rows_affected()).unwrap_or(i64::MAX);
        }
    }

    Ok(())
}
