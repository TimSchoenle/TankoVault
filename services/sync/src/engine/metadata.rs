//! The one place upstream metadata is folded into a local series, shared by [`super::enrich`]'s
//! catalogue sweep and [`super::reconcile`] so both paths resolve [`MetadataPriority`] the same way.

use tankovault_db::PgPool;
use tankovault_db::repo::catalog::{
    MetadataCandidate, MetadataEnrichment, SeriesEnrichmentRow, TagKind, TagLink, TagSource,
};
use tankovault_db::repo::{catalog, sync};
use tankovault_domain::{MetadataPriority, SeriesId, TagBlocklist, normalize_title};
use time::OffsetDateTime;

use crate::provider::RemoteMetadata;

/// Folds resolved provider metadata into the catalogue under the configured field priority.
pub(crate) struct MetadataWriter {
    pool: PgPool,
    /// Which source has the final say per metadata field (default: `AniList` over adapters).
    metadata_priority: MetadataPriority,
    /// Which scraped "genres" never become tags. The same guard the worker's ingest applies:
    /// both paths intern into one shared `tags` vocabulary.
    tag_blocklist: TagBlocklist,
}

impl MetadataWriter {
    pub(crate) const fn new(
        pool: PgPool,
        metadata_priority: MetadataPriority,
        tag_blocklist: TagBlocklist,
    ) -> Self {
        Self {
            pool,
            metadata_priority,
            tag_blocklist,
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

    /// Persist resolved provider metadata for `row`: cache the external-id mapping, offer every
    /// prioritised field to the merge, and record the upstream-only signals plus every
    /// alternative title/synonym, genre and credit.
    pub(crate) async fn apply(
        &self,
        row: &SeriesEnrichmentRow,
        slug: &str,
        meta: &RemoteMetadata,
    ) -> anyhow::Result<()> {
        sync::upsert_mapping(&self.pool, row.id, slug, &meta.external_id).await?;

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
                // Offered, not imposed: `merge_metadata` decides each field against what the
                // adapters stored and who stored it. An `Unknown` content type or status is
                // upstream having no opinion and loses to any real value.
                candidate: MetadataCandidate {
                    canonical_title: meta.titles.first().map(String::as_str),
                    description: meta.description.as_deref(),
                    cover_url: meta.cover_url.as_deref(),
                    content_type: Some(meta.content_type),
                    status: Some(meta.series_status),
                    release_year: meta.start_year,
                },
                is_adult: meta.is_adult,
                external_score: meta.external_score,
                external_popularity: meta.external_popularity,
                external_source: meta.external_source.as_deref(),
                alt_titles: &alt_titles,
                tags: &tag_links(meta),
                authors: &meta.authors,
            },
            &self.metadata_priority,
            &self.tag_blocklist,
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

/// The two upstream vocabularies as one link list: coarse genres at full strength, then the
/// provider's descriptive terms at their own rank.
///
/// Both land in `series_tags`, and `tags.kind` is what keeps them distinguishable afterwards —
/// the recommender weights a 600-term theme very differently from a genre a third of the
/// catalogue carries.
fn tag_links(meta: &RemoteMetadata) -> Vec<TagLink<'_>> {
    let mut links = Vec::with_capacity(meta.tags.len() + meta.themes.len());
    for genre in &meta.tags {
        links.push(TagLink {
            name: genre,
            kind: TagKind::Genre,
            weight: 1.0,
            source: TagSource::AniList,
        });
    }
    for theme in &meta.themes {
        links.push(TagLink {
            name: &theme.name,
            kind: TagKind::Theme,
            weight: theme.weight,
            source: TagSource::AniList,
        });
    }
    links
}
