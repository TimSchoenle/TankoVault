//! The one place upstream metadata is folded into a local series, shared by [`super::enrich`]'s
//! catalogue sweep and [`super::reconcile`] so both paths resolve [`MetadataPriority`] the same way.

use tankovault_db::PgPool;
use tankovault_db::repo::catalog::{MetadataEnrichment, SeriesEnrichmentRow};
use tankovault_db::repo::{catalog, sync};
use tankovault_domain::{
    ContentType, MetadataField, MetadataPriority, MetadataSource, SeriesId, SeriesStatus,
    normalize_title,
};
use time::OffsetDateTime;

use crate::provider::RemoteMetadata;

/// Folds resolved provider metadata into the catalogue under the configured field priority.
pub(crate) struct MetadataWriter {
    pool: PgPool,
    /// Which source has the final say per metadata field (default: `AniList` over adapters).
    metadata_priority: MetadataPriority,
}

impl MetadataWriter {
    pub(crate) const fn new(pool: PgPool, metadata_priority: MetadataPriority) -> Self {
        Self {
            pool,
            metadata_priority,
        }
    }

    /// The subset of `series_ids` whose metadata has not been attempted since `stale_before`,
    /// with the locally-stored values [`Self::apply`] needs to resolve priority against.
    pub(crate) async fn needing_metadata(
        &self,
        series_ids: &[SeriesId],
        stale_before: OffsetDateTime,
    ) -> anyhow::Result<Vec<SeriesEnrichmentRow>> {
        if series_ids.is_empty() {
            return Ok(Vec::new());
        }
        let ids: Vec<uuid::Uuid> = series_ids.iter().copied().map(SeriesId::as_uuid).collect();
        Ok(catalog::series_needing_metadata(&self.pool, &ids, stale_before).await?)
    }

    /// Persist resolved provider metadata for `row`: cache the external-id mapping, fold in the
    /// priority-resolved description/cover, gap-fill content type, publication status and
    /// release year, and record every alternative title/synonym, genre and credit.
    pub(crate) async fn apply(
        &self,
        row: &SeriesEnrichmentRow,
        slug: &str,
        meta: &RemoteMetadata,
    ) -> anyhow::Result<()> {
        sync::upsert_mapping(&self.pool, row.id, slug, &meta.external_id).await?;

        // Description/cover follow the configured priority: the provider's value versus the
        // value the scraping adapters already stored on the row.
        let description = self.metadata_priority.resolve(
            MetadataField::Description,
            &[
                (MetadataSource::AniList, meta.description.clone()),
                (MetadataSource::Adapter, row.description.clone()),
            ],
        );
        let cover = self.metadata_priority.resolve(
            MetadataField::Cover,
            &[
                (MetadataSource::AniList, meta.cover_url.clone()),
                (MetadataSource::Adapter, row.cover_url.clone()),
            ],
        );

        // Every alternative title the provider tracks (english/native/synonyms), normalized for
        // the trigram/merge/search indexes; blanks and duplicates are dropped downstream.
        let alt_titles: Vec<(String, String)> = meta
            .titles
            .iter()
            .map(|t| (t.clone(), normalize_title(t)))
            .filter(|(_, n)| !n.is_empty())
            .collect();

        catalog::apply_enrichment(
            &self.pool,
            row.id,
            &MetadataEnrichment {
                description: description.as_deref(),
                cover_url: cover.as_deref(),
                // Additive gap-fills only: never overwrite an adapter-determined value. `None`
                // means "upstream had no opinion"; an `Unknown` token would look like a real answer.
                content_type: content_type_token(meta.content_type),
                status: series_status_token(meta.series_status),
                release_year: meta.start_year,
                alt_titles: &alt_titles,
                tags: &meta.tags,
                authors: &meta.authors,
            },
        )
        .await?;
        Ok(())
    }

    /// Record that `series_id` was examined and there was nothing to write.
    ///
    /// Load-bearing: an unstamped series leads every following sweep page, so the sweep would
    /// spend its whole budget retrying it forever.
    pub(crate) async fn mark_checked(&self, series_id: SeriesId) -> anyhow::Result<()> {
        catalog::mark_metadata_checked(&self.pool, series_id).await?;
        Ok(())
    }
}

/// The enum token for a content type, or `None` when upstream had no opinion.
fn content_type_token(content_type: ContentType) -> Option<&'static str> {
    match content_type {
        ContentType::Unknown => None,
        other => Some(other.as_str()),
    }
}

/// The enum token for a publication status, or `None` when upstream had no opinion.
fn series_status_token(status: SeriesStatus) -> Option<&'static str> {
    match status {
        SeriesStatus::Unknown => None,
        other => Some(other.as_str()),
    }
}
