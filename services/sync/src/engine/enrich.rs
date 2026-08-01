//! Tokenless metadata enrichment: the only part of the engine that needs no user token, talking
//! only to providers exposing a public API. The writing half is shared with reconciliation via
//! [`super::metadata::MetadataWriter`].

use std::sync::Arc;

use serde::Serialize;
use time::OffsetDateTime;

use tankovault_db::PgPool;
use tankovault_db::repo::catalog;
use tankovault_db::repo::catalog::SeriesEnrichmentRow;
use tankovault_db::repo::sync;

use super::metadata::MetadataWriter;
use super::registry::ProviderRegistry;

/// Outcome of a tokenless metadata-enrichment sweep.
#[derive(Debug, Default, Serialize)]
pub(crate) struct EnrichReport {
    /// Series examined this sweep.
    pub(crate) scanned: usize,
    /// Series that received metadata from at least one provider.
    pub(crate) enriched: usize,
    /// Series no public provider could resolve.
    pub(crate) unresolved: usize,
}

/// Folds public provider metadata into the local catalogue.
pub(crate) struct Enricher {
    pool: PgPool,
    registry: Arc<ProviderRegistry>,
    metadata: Arc<MetadataWriter>,
}

impl Enricher {
    pub(crate) const fn new(
        pool: PgPool,
        registry: Arc<ProviderRegistry>,
        metadata: Arc<MetadataWriter>,
    ) -> Self {
        Self {
            pool,
            registry,
            metadata,
        }
    }

    /// Walk the catalogue in batches of `batch_size`, up to `max_series` series, asking every
    /// public-metadata provider for each one's catalogue metadata by cached external id, else
    /// by canonical title. Never fails the whole sweep on a single series' error.
    pub(crate) async fn enrich_all(
        &self,
        batch_size: i64,
        max_series: usize,
    ) -> anyhow::Result<EnrichReport> {
        let mut report = EnrichReport::default();
        if !self.registry.any_public_metadata() {
            return Ok(report);
        }
        // Every row touched is stamped `metadata_checked_at = now()`, success or not, and
        // `started_at` fences the run — that stamp is the whole paging mechanism. Paging on
        // `updated_at` instead (only a success writes it) let unresolvable series stay at the
        // head forever, starving pages once they outnumbered `max_series`.
        let started_at = OffsetDateTime::now_utc();
        while report.scanned < max_series {
            let rows =
                catalog::list_series_for_enrichment(&self.pool, batch_size, started_at).await?;
            if rows.is_empty() {
                break;
            }
            let fetched = rows.len();
            for row in rows {
                if report.scanned >= max_series {
                    break;
                }
                report.scanned += 1;
                match self.enrich_series(&row).await {
                    Ok(true) => report.enriched += 1,
                    Ok(false) => {
                        report.unresolved += 1;
                        self.mark_checked(&row).await;
                    }
                    Err(e) => {
                        report.unresolved += 1;
                        tracing::warn!(error = %e, series_id = %row.id, "series enrichment failed");
                        self.mark_checked(&row).await;
                    }
                }
            }
            if i64::try_from(fetched).unwrap_or(0) < batch_size {
                break;
            }
        }
        tracing::info!(
            scanned = report.scanned,
            enriched = report.enriched,
            unresolved = report.unresolved,
            "tokenless metadata enrichment sweep complete"
        );
        Ok(report)
    }

    /// Enrich one series from the first public provider that resolves it. Returns whether any
    /// provider supplied metadata.
    async fn enrich_series(&self, row: &SeriesEnrichmentRow) -> anyhow::Result<bool> {
        for (slug, provider) in self.registry.iter() {
            if !provider.supports_public_metadata() {
                continue;
            }
            let existing = sync::mapping_external_for_series(&self.pool, row.id, slug).await?;
            let meta = match existing {
                Some(ext) => provider.fetch_public_metadata_by_id(&ext).await?,
                None => {
                    provider
                        .fetch_public_metadata_by_title(&row.canonical_title)
                        .await?
                }
            };
            let Some(meta) = meta else {
                continue;
            };
            self.metadata.apply(row, slug, &meta).await?;
            return Ok(true);
        }
        Ok(false)
    }

    /// Stamp a series the sweep got nothing out of, so the next page moves past it.
    ///
    /// A failure to stamp is logged rather than propagated: the sweep would otherwise abort a
    /// whole run over one row, and the run's remaining budget is worth more than the row.
    async fn mark_checked(&self, row: &SeriesEnrichmentRow) {
        if let Err(e) = self.metadata.mark_checked(row.id).await {
            tracing::warn!(error = %e, series_id = %row.id, "could not stamp enrichment attempt");
        }
    }
}
