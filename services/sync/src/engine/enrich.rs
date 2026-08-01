//! Tokenless metadata enrichment (design: worker queue syncing every existing entry to
//! `AniList` **without** a stored user token).
//!
//! The one part of the engine that needs no user token at all — it talks only to providers that
//! expose a public API (`ExternalProvider::supports_public_metadata`). Keeping it a separate
//! collaborator is what stops it from being welded to token-bearing reconciliation, which is
//! how ARCH-6 described the old arrangement. The *writing* half is shared with reconciliation
//! all the same: see [`super::metadata::MetadataWriter`].

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

    /// Walk the catalogue in batches of `batch_size`, up to `max_series` series, and for each
    /// one ask every provider that exposes a public API (`AniList`'s unauthenticated GraphQL)
    /// for its catalogue metadata — by an already-cached external id where one exists, else by
    /// canonical title. Resolved metadata is folded in under the configured per-field priority,
    /// and every alternative title/synonym is persisted for merge detection and search.
    ///
    /// Never fails the whole sweep on a single series' error — those are logged and skipped.
    pub(crate) async fn enrich_all(
        &self,
        batch_size: i64,
        max_series: usize,
    ) -> anyhow::Result<EnrichReport> {
        let mut report = EnrichReport::default();
        if !self.registry.any_public_metadata() {
            return Ok(report);
        }
        // The work list is "least recently attempted first" and every series this loop touches
        // is stamped `metadata_checked_at = now()` — including the ones nothing resolved and the
        // ones that errored. That stamp is the whole paging mechanism: `started_at` fences the
        // run, so a stamped row fails the next page's predicate and cannot come back around.
        //
        // It is also why there is no cursor here any more. Paging used to key on `updated_at`,
        // which only a *successful* enrichment writes, so a series no provider could resolve
        // stayed at the head of the ordering and was retried at the front of every sweep. A
        // catalogue with more unresolvable series than `max_series` never advanced past them.
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
