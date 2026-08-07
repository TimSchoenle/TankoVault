//! Matching a remote entry to a canonical local series and back again, caching each answer in
//! `sync_mappings`. Thresholds and candidate limit are the same `MatchingConfig` the worker's
//! ingest canonicalisation reads, so the two paths can't disagree about what's confident.

use std::collections::{HashMap, HashSet};

use secrecy::SecretString;
use serde_json::json;
use tankovault_db::PgPool;
use tankovault_db::repo::{catalog, matching, sync};
use tankovault_domain::{SeriesId, normalize_title};
use tankovault_matcher::{Assessment, Query, Thresholds, best_assessment, explain};

use crate::provider::{ExternalProvider, RemoteEntry};

/// How a remote entry was resolved — or why it was not.
///
/// The whole value is journalled. Before it, a mapping was written from a bare `Option<SeriesId>`:
/// the score that justified it, which of the entry's titles matched, and the runner-up it beat
/// all existed for the length of one function call and were then discarded, so a wrong mapping
/// could be seen but never explained.
#[derive(Debug, Clone)]
pub(crate) struct MatchOutcome {
    pub(crate) series_id: Option<SeriesId>,
    /// Stable slug: `existing_mapping`, `title_match_above_threshold`,
    /// `below_match_threshold`, `numeric_conflict`, `blocked_by_operator`, `no_candidates`.
    pub(crate) reason: &'static str,
    /// The winning assessment, absent when the entry resolved from a cached mapping (which is
    /// not a fresh judgement) or when nothing scored at all.
    pub(crate) assessment: Option<Assessment>,
    /// Which titles matched, every term of the score, and the runner-up.
    pub(crate) evidence: serde_json::Value,
}

impl MatchOutcome {
    fn from_mapping(series_id: SeriesId) -> Self {
        Self {
            series_id: Some(series_id),
            reason: "existing_mapping",
            assessment: None,
            evidence: json!({ "source": "sync_mappings" }),
        }
    }
}

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

    /// Every (external id, series) pair an operator has judged wrong for this provider.
    ///
    /// Read once per run and passed into [`Self::series_for_entry`] rather than queried per
    /// entry: the list is small, and the alternative is a round trip inside the resolution loop.
    ///
    /// # Errors
    /// Database failures.
    pub(crate) async fn blocklist(
        &self,
        slug: &str,
    ) -> anyhow::Result<HashSet<(String, SeriesId)>> {
        Ok(sync::blocked_sync_matches(&self.pool, slug)
            .await?
            .into_iter()
            .collect())
    }

    /// Resolve a remote entry to a canonical series: first via an existing mapping, then by
    /// the best confident title match against the local catalogue.
    ///
    /// Every candidate title is scored independently and the global best is kept, so the entry
    /// attaches if *any* title matches confidently, not just the first tried. Titles are
    /// deduplicated by normalized form first, since synonym lists routinely repeat each other.
    #[expect(
        clippy::too_many_lines,
        reason = "the scoring loop and the five refusals that follow it are one decision, and \
                  each refusal returns the same evidence assembled once above it. Splitting \
                  them would either rebuild that evidence per arm or hand it around as a \
                  parameter, both of which make it easier for one arm to report different \
                  evidence from the rest"
    )]
    pub(crate) async fn series_for_entry(
        &self,
        slug: &str,
        entry: &RemoteEntry,
        blocked: &HashSet<(String, SeriesId)>,
    ) -> anyhow::Result<MatchOutcome> {
        if let Some(id) =
            sync::mapping_series_for_external(&self.pool, slug, entry.external_id()).await?
        {
            return Ok(MatchOutcome::from_mapping(id));
        }

        let mut seen = HashSet::with_capacity(entry.metadata.titles.len());
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

        let mut best: Option<(SeriesId, Assessment, String)> = None;
        let mut runner_up: Option<(SeriesId, f32)> = None;
        let mut candidates_seen = 0_usize;
        // Kept so the journal can name the losing candidate: an operator looking at a wrong match
        // almost always wants to know what it was competing with.
        let mut by_id: HashMap<SeriesId, tankovault_matcher::Candidate> = HashMap::new();

        for (normalized, candidates) in per_title {
            candidates_seen += candidates.len();
            for candidate in &candidates {
                by_id
                    .entry(candidate.series_id)
                    .or_insert_with(|| candidate.clone());
            }
            // Shared `Candidate` type, not a per-path field conversion: keeps a new field from
            // silently reaching one matching path and not the other.
            // Remote genres/staff matched against each candidate's local tags/authors give the
            // extra signal that resolves ambiguous title matches.
            let query = Query {
                normalized_title: normalized.clone(),
                content_type: entry.metadata.content_type,
                release_year: entry.metadata.start_year,
                tags: entry.metadata.tags.clone(),
                authors: entry.metadata.authors.clone(),
            };
            if let Some((id, assessment)) = best_assessment(&query, &candidates) {
                match &best {
                    Some((_, b, _)) if assessment.score <= b.score => {
                        if runner_up.is_none_or(|(_, s)| assessment.score > s) {
                            runner_up = Some((id, assessment.score));
                        }
                    }
                    _ => {
                        if let Some((prev_id, prev, _)) = best.take() {
                            runner_up = Some((prev_id, prev.score));
                        }
                        best = Some((id, assessment, normalized));
                    }
                }
            }
        }

        let Some((id, assessment, matched_title)) = best else {
            return Ok(MatchOutcome {
                series_id: None,
                reason: "no_candidates",
                assessment: None,
                evidence: json!({
                    "titles_tried": normalized_titles,
                    "candidates_seen": candidates_seen,
                }),
            });
        };

        // The itemised score, computed once for the winner only. Explaining every candidate would
        // allocate a term list per candidate per title for numbers that are then discarded.
        let winning_query = Query {
            normalized_title: matched_title.clone(),
            content_type: entry.metadata.content_type,
            release_year: entry.metadata.start_year,
            tags: entry.metadata.tags.clone(),
            authors: entry.metadata.authors.clone(),
        };
        let explanation = by_id.get(&id).map(|c| explain(&winning_query, c));

        let evidence = json!({
            "titles_tried": normalized_titles,
            "candidates_seen": candidates_seen,
            "matched_query_title": matched_title,
            "matched_candidate_title": explanation.as_ref().map(|e| e.matched_candidate_title.clone()),
            "via_alias": explanation.as_ref().map(|e| e.via_alias),
            "base_score": explanation.as_ref().map(|e| e.base),
            "terms": explanation.as_ref().map(|e| e.terms.iter()
                .map(|t| json!({ "rule": t.rule, "delta": t.delta, "detail": t.detail }))
                .collect::<Vec<_>>()),
            "runner_up": runner_up.map(|(rid, score)| json!({ "series_id": rid, "score": score })),
            "thresholds": { "attach": self.thresholds.high },
            "remote": {
                "external_id": entry.external_id(),
                "titles": entry.metadata.titles,
                "content_type": entry.metadata.content_type.as_str(),
                "start_year": entry.metadata.start_year,
            },
        });

        // An operator's refusal outranks the score. Checked after scoring rather than before, so
        // the journal records what *would* have matched — a blocklist entry that is suppressing a
        // 0.99 match is worth seeing, and one suppressing a 0.87 one is worth reconsidering.
        if blocked.contains(&(entry.external_id().to_owned(), id)) {
            return Ok(MatchOutcome {
                series_id: None,
                reason: "blocked_by_operator",
                assessment: Some(assessment),
                evidence,
            });
        }

        // The numeric veto applies here for the same reason it applies at ingest, and this path
        // needs it more: a tracker library is full of numbered sequels sitting next to their
        // predecessors, and mapping `Overlord 2` onto the local `Overlord` writes a
        // `sync_mappings` row that then pushes one series' progress onto the other's.
        if assessment.signals.numeric_conflict {
            return Ok(MatchOutcome {
                series_id: None,
                reason: "numeric_conflict",
                assessment: Some(assessment),
                evidence,
            });
        }
        if assessment.score < self.thresholds.high {
            return Ok(MatchOutcome {
                series_id: None,
                reason: "below_match_threshold",
                assessment: Some(assessment),
                evidence,
            });
        }
        Ok(MatchOutcome {
            series_id: Some(id),
            reason: "title_match_above_threshold",
            assessment: Some(assessment),
            evidence,
        })
    }

    /// Resolve a local series to a `provider` external id: via an existing mapping, else by a
    /// title search (whose result is cached as a mapping).
    ///
    /// The search result is checked against the blocklist before it is cached: a provider search
    /// is a weaker signal than the scored title match above, so a match an operator has already
    /// rejected must not come back through the side door.
    pub(crate) async fn media_id_for_series(
        &self,
        provider: &dyn ExternalProvider,
        slug: &str,
        access: &SecretString,
        series_id: SeriesId,
        blocked: &HashSet<(String, SeriesId)>,
    ) -> anyhow::Result<Option<String>> {
        if let Some(ext) = sync::mapping_external_for_series(&self.pool, series_id, slug).await? {
            return Ok(Some(ext));
        }
        let series = catalog::get_series(&self.pool, series_id).await?;
        if let Some(id) = provider.search(access, &series.canonical_title).await? {
            if blocked.contains(&(id.clone(), series_id)) {
                return Ok(None);
            }
            sync::upsert_mapping(&self.pool, series_id, slug, &id).await?;
            return Ok(Some(id));
        }
        Ok(None)
    }
}
