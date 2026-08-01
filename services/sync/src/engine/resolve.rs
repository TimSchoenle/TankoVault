//! Matching a remote entry to a canonical local series and back again, caching each answer in
//! `sync_mappings`. Thresholds and candidate limit are the same `MatchingConfig` the worker's
//! ingest canonicalisation reads, so the two paths can't disagree about what's confident.

use secrecy::SecretString;
use tankovault_db::PgPool;
use tankovault_db::repo::{catalog, matching, sync};
use tankovault_domain::{SeriesId, normalize_title};
use tankovault_matcher::{Assessment, Query, Thresholds, best_assessment};

use crate::provider::{ExternalProvider, RemoteEntry};

/// Resolves the series ⇆ external-id correspondence in both directions.
pub(crate) struct SeriesResolver {
    pool: PgPool,
    thresholds: Thresholds,
    candidate_limit: i64,
}

impl SeriesResolver {
    pub(crate) const fn new(pool: PgPool, thresholds: Thresholds, candidate_limit: i64) -> Self {
        Self {
            pool,
            thresholds,
            candidate_limit,
        }
    }

    /// Resolve a remote entry to a canonical series: first via an existing mapping, then by
    /// the best confident title match against the local catalogue.
    ///
    /// Every candidate title is scored independently and the global best is kept, so the entry
    /// attaches if *any* title matches confidently, not just the first tried. Titles are
    /// deduplicated by normalized form first, since synonym lists routinely repeat each other.
    pub(crate) async fn series_for_entry(
        &self,
        slug: &str,
        entry: &RemoteEntry,
    ) -> anyhow::Result<Option<SeriesId>> {
        if let Some(id) =
            sync::mapping_series_for_external(&self.pool, slug, entry.external_id()).await?
        {
            return Ok(Some(id));
        }

        let mut seen = std::collections::HashSet::with_capacity(entry.metadata.titles.len());
        let normalized_titles: Vec<String> = entry
            .metadata
            .titles
            .iter()
            .map(|title| normalize_title(title))
            .filter(|normalized| !normalized.is_empty() && seen.insert(normalized.clone()))
            .collect();

        // One round trip for the whole title family rather than one trigram scan per title.
        let per_title =
            matching::find_candidates_multi(&self.pool, &normalized_titles, self.candidate_limit)
                .await?;

        let mut best: Option<(SeriesId, Assessment)> = None;
        for (normalized, candidates) in per_title {
            // Shared `Candidate` type, not a per-path field conversion: keeps a new field from
            // silently reaching one matching path and not the other.
            // Remote genres/staff matched against each candidate's local tags/authors give the
            // extra signal that resolves ambiguous title matches.
            let query = Query {
                normalized_title: normalized,
                content_type: entry.metadata.content_type,
                release_year: entry.metadata.start_year,
                tags: entry.metadata.tags.clone(),
                authors: entry.metadata.authors.clone(),
            };
            if let Some((id, assessment)) = best_assessment(&query, &candidates)
                && best.is_none_or(|(_, b)| assessment.score > b.score)
            {
                best = Some((id, assessment));
            }
        }

        // The numeric veto applies here for the same reason it applies at ingest, and this path
        // needs it more: a tracker library is full of numbered sequels sitting next to their
        // predecessors, and mapping `Overlord 2` onto the local `Overlord` writes a
        // `sync_mappings` row that then pushes one series' progress onto the other's.
        Ok(best
            .filter(|(_, a)| a.score >= self.thresholds.high && !a.signals.numeric_conflict)
            .map(|(id, _)| id))
    }

    /// Resolve a local series to a `provider` external id: via an existing mapping, else by a
    /// title search (whose result is cached as a mapping).
    pub(crate) async fn media_id_for_series(
        &self,
        provider: &dyn ExternalProvider,
        slug: &str,
        access: &SecretString,
        series_id: SeriesId,
    ) -> anyhow::Result<Option<String>> {
        if let Some(ext) = sync::mapping_external_for_series(&self.pool, series_id, slug).await? {
            return Ok(Some(ext));
        }
        let series = catalog::get_series(&self.pool, series_id).await?;
        if let Some(id) = provider.search(access, &series.canonical_title).await? {
            sync::upsert_mapping(&self.pool, series_id, slug, &id).await?;
            return Ok(Some(id));
        }
        Ok(None)
    }
}
