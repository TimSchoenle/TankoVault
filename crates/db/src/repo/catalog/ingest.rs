//! Composite ingest (worker `series` task): one transaction, idempotent, returning the
//! genuinely new chapters for the `chapter.discovered` fan-out.

use super::chapters::{ChapterUpsert, upsert_chapters};
use super::enrichment::{add_series_authors, add_series_tags, add_series_titles};
use super::series::{SeriesUpsert, resolve_canonical_series, update_series_meta};
use super::sources::{update_source_scan, upsert_source};
use crate::error::DbResult;
use tankovault_config::MatchingConfig;
use tankovault_domain::{ProviderId, SeriesId, SeriesSourceId};

/// A fully-scanned series ready to persist: canonical metadata, alternative titles,
/// and the full chapter list, plus the content hash for change detection.
pub struct ScannedSeries {
    pub provider_id: ProviderId,
    pub source_path: String,
    pub provider_title: Option<String>,
    pub meta: SeriesUpsert,
    pub alt_titles: Vec<(String, String)>,
    pub tags: Vec<String>,
    pub authors: Vec<String>,
    pub chapters: Vec<ChapterUpsert>,
    pub content_hash: Vec<u8>,
}

/// Result of ingesting a scanned series.
pub struct IngestOutcome {
    pub series_id: SeriesId,
    pub source_id: SeriesSourceId,
    /// Newly-discovered chapter numbers, in input order (for `chapter.discovered`).
    pub new_chapters: Vec<f64>,
}

/// Persist a scanned series and its chapters in a single transaction.
///
/// All writes are idempotent (`ON CONFLICT`), so replaying a task under at-least-once
/// delivery converges to the same state and reports no false-new chapters.
pub async fn ingest_series(
    pool: &sqlx::PgPool,
    scanned: ScannedSeries,
    matching: &MatchingConfig,
) -> DbResult<IngestOutcome> {
    let mut tx = pool.begin().await?;

    let series_id = resolve_canonical_series(&mut tx, &scanned.meta, matching).await?;
    update_series_meta(&mut *tx, series_id, &scanned.meta).await?;
    if !scanned.alt_titles.is_empty() {
        add_series_titles(&mut tx, series_id, &scanned.alt_titles).await?;
    }
    if !scanned.tags.is_empty() {
        add_series_tags(&mut tx, series_id, &scanned.tags).await?;
    }
    if !scanned.authors.is_empty() {
        add_series_authors(&mut tx, series_id, &scanned.authors).await?;
    }

    let source_id = upsert_source(
        &mut *tx,
        series_id,
        scanned.provider_id,
        &scanned.source_path,
        scanned.provider_title.as_deref(),
    )
    .await?;

    // One statement, not one per chapter. A series with two thousand chapters used to mean
    // two thousand sequential round trips inside this transaction â€” which also holds row
    // locks on the shared `tags`/`authors` rows, so a single slow series stalled every other
    // provider's ingest behind it.
    let mut new_chapters = upsert_chapters(&mut *tx, source_id, &scanned.chapters).await?;
    // The row-at-a-time loop emitted these in listing order; `RETURNING` does not promise
    // any order. `chapter.discovered` consumers do not depend on it, but a stable order
    // keeps the notification stream and the tests deterministic.
    new_chapters.sort_by(f64::total_cmp);

    let count = i32::try_from(scanned.chapters.len()).unwrap_or(i32::MAX);
    update_source_scan(&mut *tx, source_id, &scanned.content_hash, count).await?;

    tx.commit().await?;
    Ok(IngestOutcome {
        series_id,
        source_id,
        new_chapters,
    })
}
