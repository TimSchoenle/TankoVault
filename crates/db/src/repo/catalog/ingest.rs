//! Composite ingest (worker `series` task): one transaction, idempotent, returning the
//! genuinely new chapters for the `chapter.discovered` fan-out.

use super::chapters::{ChapterUpsert, upsert_chapters};
use super::enrichment::{TagLink, add_series_authors, add_series_tags, add_series_titles};
use super::metadata::merge_metadata;
use super::series::{SeriesUpsert, resolve_canonical_series};
use super::sources::{update_source_scan, upsert_source};
use crate::error::DbResult;
use tankovault_domain::matching::Canonicaliser;
use tankovault_domain::{MetadataPriority, MetadataSource, ProviderId, SeriesId, SeriesSourceId};

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

/// Persist a scanned series and its chapters in a single transaction. All writes are
/// idempotent, so replaying under at-least-once delivery converges without false-new chapters.
///
/// `canonicaliser` decides which series the scan belongs to; `priority` decides which of the
/// scan's values are allowed to replace what another source already wrote. This function only
/// writes.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; any failure rolls back the whole transaction, so a series
/// never exists with only some of its chapters. A crash after commit but before the caller
/// publishes `chapter.discovered` can still lose events — a replay must not report false-new
/// chapters.
pub async fn ingest_series(
    pool: &sqlx::PgPool,
    scanned: &ScannedSeries,
    canonicaliser: &dyn Canonicaliser,
    priority: &MetadataPriority,
) -> DbResult<IngestOutcome> {
    let mut tx = pool.begin().await?;

    let series_id = resolve_canonical_series(&mut tx, &scanned.meta, canonicaliser).await?;
    merge_metadata(
        &mut tx,
        series_id,
        MetadataSource::Adapter,
        &scanned.meta.candidate(),
        priority,
    )
    .await?;
    if !scanned.alt_titles.is_empty() {
        add_series_titles(&mut tx, series_id, &scanned.alt_titles).await?;
    }
    if !scanned.tags.is_empty() {
        // A scraped tag is a bare genre name: no rank to carry, so it is wholly present.
        let links: Vec<TagLink<'_>> = scanned.tags.iter().map(|t| TagLink::genre(t)).collect();
        add_series_tags(&mut tx, series_id, &links).await?;
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

    // One statement, not a per-chapter loop: this transaction holds row locks on shared
    // `tags`/`authors` rows, so per-chapter round trips would stall other providers' ingests.
    let mut new_chapters = upsert_chapters(&mut *tx, source_id, &scanned.chapters).await?;
    // `RETURNING` doesn't promise order; sort for a deterministic notification stream.
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
