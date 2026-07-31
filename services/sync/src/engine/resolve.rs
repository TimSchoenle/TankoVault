//! Matching a remote entry to a canonical local series and back again.
//!
//! Both directions cache their answer in `sync_mappings`, so a later run skips the match
//! entirely. The thresholds and the candidate limit come from `MatchingConfig` — the *same*
//! configuration the worker's ingest canonicalisation reads, so the two paths cannot disagree
//! about what counts as a confident match (ARCH-16).

use tankovault_db::PgPool;
use tankovault_db::repo::{catalog, matching, sync};
use tankovault_domain::{SeriesId, normalize_title};
use tankovault_matcher::{Query, Thresholds, best_match};

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
    /// Every candidate title (romaji/english/native, plus every `AniList` synonym) is scored
    /// against its own trigram candidates and the **global** best is taken, so an entry
    /// attaches when *any* of its titles matches confidently — not just the first one tried.
    /// Synonym lists routinely duplicate the official titles or each other once normalized
    /// (case, punctuation, a "manga"/"webtoon" suffix), so titles are deduplicated by their
    /// normalized form first — one DB round trip per distinct key, not per raw string.
    pub(crate) async fn series_for_entry(
        &self,
        slug: &str,
        entry: &RemoteEntry,
    ) -> anyhow::Result<Option<SeriesId>> {
        if let Some(id) =
            sync::mapping_series_for_external(&self.pool, slug, &entry.external_id).await?
        {
            return Ok(Some(id));
        }

        let mut seen = std::collections::HashSet::with_capacity(entry.titles.len());
        let normalized_titles: Vec<String> = entry
            .titles
            .iter()
            .map(|title| normalize_title(title))
            .filter(|normalized| !normalized.is_empty() && seen.insert(normalized.clone()))
            .collect();

        // One round trip for the whole title family rather than one per title (PERF-13): an
        // `AniList` entry routinely carries 3-8 distinct normalized titles, each of which was a
        // separate trigram scan.
        let per_title =
            matching::find_candidates_multi(&self.pool, &normalized_titles, self.candidate_limit)
                .await?;

        let mut best: Option<(SeriesId, f32)> = None;
        for (normalized, candidates) in per_title {
            // No conversion: `find_candidates_multi` already yields the scorer's own
            // [`Candidate`], the single type both this path and the worker's ingest
            // canonicalisation score. It used to be a `crates/db` row struct converted field
            // for field in each of the two places, so a new candidate field silently reached
            // one path and not the other (ARCH-16).
            // AniList's own genres/staff, matched against each candidate's locally-scraped
            // tags/authors — the extra signal that makes ambiguous title matches confident.
            let query = Query {
                normalized_title: normalized,
                content_type: entry.content_type,
                release_year: entry.start_year,
                tags: entry.tags.clone(),
                authors: entry.authors.clone(),
            };
            if let Some((id, score)) = best_match(&query, &candidates) {
                if best.is_none_or(|(_, b)| score > b) {
                    best = Some((id, score));
                }
            }
        }

        Ok(best
            .filter(|(_, score)| *score >= self.thresholds.high)
            .map(|(id, _)| id))
    }

    /// Resolve a local series to a `provider` external id: via an existing mapping, else by a
    /// title search (whose result is cached as a mapping).
    pub(crate) async fn media_id_for_series(
        &self,
        provider: &dyn ExternalProvider,
        slug: &str,
        access: &str,
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
