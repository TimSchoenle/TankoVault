//! Tokenless metadata enrichment (design: worker queue syncing every existing entry to
//! `AniList` **without** a stored user token).
//!
//! The one part of the engine that needs no user token at all — it talks only to providers that
//! expose a public API (`ExternalProvider::supports_public_metadata`). Keeping it a separate
//! collaborator is what stops it from being welded to token-bearing reconciliation, which is
//! how ARCH-6 described the old arrangement.

use std::sync::Arc;

use serde::Serialize;
use time::OffsetDateTime;

use tankovault_config::{MetadataPriorityConfig, SOURCE_ADAPTER, SOURCE_ANILIST};
use tankovault_db::PgPool;
use tankovault_db::repo::catalog::{MetadataEnrichment, SeriesEnrichmentRow};
use tankovault_db::repo::{catalog, sync};
use tankovault_domain::normalize_title;

use super::registry::ProviderRegistry;
use crate::provider::RemoteMetadata;

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
    /// Which source has the final say per metadata field (default: `AniList` over adapters).
    metadata_priority: MetadataPriorityConfig,
}

impl Enricher {
    pub(crate) const fn new(
        pool: PgPool,
        registry: Arc<ProviderRegistry>,
        metadata_priority: MetadataPriorityConfig,
    ) -> Self {
        Self {
            pool,
            registry,
            metadata_priority,
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
        // A keyset walk, not `OFFSET`. Enrichment writes `updated_at = now()`, which is the
        // very column the sweep ordered by — so with `OFFSET` every enriched row jumped to
        // the end of the ordering, the rows behind it shifted forward, and the next page's
        // offset skipped exactly those. The sweep silently missed series.
        //
        // `started_at` fences the run: a row this sweep has already touched now sorts after
        // it *and* fails `updated_at < started_at`, so it cannot come back around.
        let started_at = OffsetDateTime::now_utc();
        let mut cursor: Option<(OffsetDateTime, uuid::Uuid)> = None;
        while report.scanned < max_series {
            let rows =
                catalog::list_series_for_enrichment(&self.pool, batch_size, cursor, started_at)
                    .await?;
            if rows.is_empty() {
                break;
            }
            let fetched = rows.len();
            for row in rows {
                if report.scanned >= max_series {
                    break;
                }
                // Advanced before the work, not after: an enrichment that fails must still
                // move the cursor, or a permanently-failing row stalls the sweep forever.
                cursor = Some((row.updated_at, row.id.as_uuid()));
                report.scanned += 1;
                match self.enrich_series(&row).await {
                    Ok(true) => report.enriched += 1,
                    Ok(false) => report.unresolved += 1,
                    Err(e) => {
                        report.unresolved += 1;
                        tracing::warn!(error = %e, series_id = %row.id, "series enrichment failed");
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
            self.apply_metadata(row, slug, &meta).await?;
            return Ok(true);
        }
        Ok(false)
    }

    /// Persist resolved public metadata for `row`: cache the external-id mapping, fold in the
    /// priority-resolved description/cover, and record every alternative title/synonym, genre
    /// and credit.
    async fn apply_metadata(
        &self,
        row: &SeriesEnrichmentRow,
        slug: &str,
        meta: &RemoteMetadata,
    ) -> anyhow::Result<()> {
        sync::upsert_mapping(&self.pool, row.id, slug, &meta.external_id).await?;

        // Description/cover follow the configured priority: the AniList value versus the
        // value the scraping adapters already stored on the row.
        let description = self.metadata_priority.resolve(
            "description",
            &[
                (SOURCE_ANILIST, meta.description.clone()),
                (SOURCE_ADAPTER, row.description.clone()),
            ],
        );
        let cover = self.metadata_priority.resolve(
            "cover",
            &[
                (SOURCE_ANILIST, meta.cover_url.clone()),
                (SOURCE_ADAPTER, row.cover_url.clone()),
            ],
        );

        // Every alternative title AniList tracks (english/native/synonyms), normalized for
        // the trigram/merge/search indexes; blanks and duplicates are dropped downstream.
        let alt_titles: Vec<(String, String)> = meta
            .titles
            .iter()
            .map(|t| (t.clone(), normalize_title(t)))
            .filter(|(_, n)| !n.is_empty())
            .collect();

        // Content-type and release year are additive gap-fills (never overwrite a value the
        // adapters already determined), so the AniList value only lands where local data is
        // missing — no priority resolution needed.
        let content_type = match meta.content_type {
            tankovault_domain::ContentType::Unknown => None,
            other => Some(other.as_str()),
        };

        catalog::apply_enrichment(
            &self.pool,
            row.id,
            &MetadataEnrichment {
                description: description.as_deref(),
                cover_url: cover.as_deref(),
                content_type,
                release_year: meta.start_year,
                alt_titles: &alt_titles,
                tags: &meta.tags,
                authors: &meta.authors,
            },
        )
        .await?;
        Ok(())
    }
}
