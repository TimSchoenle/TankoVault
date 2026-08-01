//! The standing duplicate sweep: finds series the catalogue holds twice, merges the
//! certain ones, and queues the rest for review, using richer signals than were available
//! when the series were first ingested.

use std::collections::{HashMap, HashSet};

use tankovault_config::MatchingConfig;
use tankovault_contracts::admin::MergeSweepView;
use tankovault_db::PgPool;
use tankovault_db::repo::matching::{self, DuplicatePair, SeriesMatchFacts};
use tankovault_domain::matching::MergeVerdict;
use tankovault_domain::{SeriesId, UserId};
use tankovault_matcher::{Assessment, Candidate, Query, adjudicate, assess};

/// How much work one sweep may do. `max_auto_merges` bounds a background action that deletes
/// rows — without it, a bad threshold change could collapse the whole catalogue in one run.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SweepBudget {
    /// Newly-blocked pairs to shortlist.
    pub pairs: i64,
    /// Open queue rows to re-score, oldest-refreshed first.
    pub requeue: i64,
    /// Automatic merges permitted in this run.
    pub max_auto_merges: i64,
}

/// Run one sweep.
///
/// `actor` is the operator who triggered it, or `None` for the scheduled run — recorded
/// in `merge_candidates.resolved_by` for attribution.
///
/// # Errors
///
/// Database failures; merges already committed stay committed (idempotent, safe to re-run).
pub(crate) async fn sweep(
    pool: &PgPool,
    policy: &MatchingConfig,
    budget: SweepBudget,
    actor: Option<UserId>,
) -> anyhow::Result<MergeSweepView> {
    let mut pairs = matching::find_duplicate_pairs(pool, budget.pairs).await?;
    pairs.extend(matching::open_merge_pairs(pool, budget.requeue).await?);
    pairs.sort_unstable();
    pairs.dedup();
    if pairs.is_empty() {
        return Ok(MergeSweepView::default());
    }

    // Batched reads for the whole shortlist. `pair_similarities` is per-pair, not
    // per-series — omitting it would re-score a trigram-matched row lower on no new evidence.
    let facts = load_facts(pool, &pairs).await?;
    let similarity: HashMap<DuplicatePair, f32> = matching::pair_similarities(pool, &pairs)
        .await?
        .into_iter()
        .collect();

    let thresholds = policy.thresholds();
    let mut report = MergeSweepView::default();
    let mut absorbed: HashSet<SeriesId> = HashSet::new();

    for pair in pairs {
        let (a, b) = pair;
        // Facts loaded for an already-merged series are stale; skip it for the rest of the
        // run — chains resolve one link per sweep.
        if absorbed.contains(&a) || absorbed.contains(&b) {
            continue;
        }
        let (Some(left), Some(right)) = (facts.get(&a), facts.get(&b)) else {
            continue;
        };
        report.pairs_examined += 1;

        let assessment = symmetric_assessment(
            left,
            right,
            similarity.get(&pair).copied().unwrap_or_default(),
        );

        match adjudicate(assessment, thresholds) {
            MergeVerdict::Auto => {
                if report.auto_merged >= budget.max_auto_merges {
                    report.deferred += 1;
                    continue;
                }
                // Survivor keeps more of the catalogue. The absorbed id stops existing,
                // breaking any bookmark, notification or tracker mapping that named it directly.
                let (keep, drop) = if left.weight() >= right.weight() {
                    (left, right)
                } else {
                    (right, left)
                };
                matching::merge_series(pool, keep.series_id, drop.series_id, actor, "auto_merged")
                    .await?;
                absorbed.insert(drop.series_id);
                report.auto_merged += 1;
                // Info-level with both titles and signals: this is a destructive, human-free
                // action, so the log line must be enough to audit the decision after the fact.
                tracing::info!(
                    keep = %keep.series_id,
                    keep_title = %keep.canonical_title,
                    dropped = %drop.series_id,
                    dropped_title = %drop.canonical_title,
                    score = assessment.score,
                    signals = ?assessment.signals.labels(),
                    "auto-merged duplicate series"
                );
            }
            MergeVerdict::Review => {
                let labels = assessment.signals.labels();
                if matching::record_merge_candidate(
                    pool,
                    a,
                    b,
                    assessment.score,
                    &labels,
                    "duplicate sweep",
                )
                .await?
                {
                    report.queued += 1;
                }
            }
            MergeVerdict::Distinct => {
                if matching::withdraw_merge_candidate(pool, a, b).await? {
                    report.withdrawn += 1;
                } else {
                    report.distinct += 1;
                }
            }
        }
    }

    Ok(report)
}

/// Load [`SeriesMatchFacts`] for every series named by `pairs`, keyed by id.
async fn load_facts(
    pool: &PgPool,
    pairs: &[DuplicatePair],
) -> anyhow::Result<HashMap<SeriesId, SeriesMatchFacts>> {
    let mut ids: Vec<SeriesId> = pairs.iter().flat_map(|(a, b)| [*a, *b]).collect();
    ids.sort_unstable();
    ids.dedup();

    let mut out = HashMap::with_capacity(ids.len());
    // Chunked so a large shortlist does not build one enormous array parameter.
    for chunk in ids.chunks(500) {
        for facts in matching::series_match_facts(pool, chunk).await? {
            out.insert(facts.series_id, facts);
        }
    }
    Ok(out)
}

/// Score the pair in **both** directions and keep the better answer.
///
/// `Query` carries no alternative titles, so `assess(left, right)` sees only the right side's
/// synonyms. Correct for a scan (the incoming source genuinely has one title), but here both
/// sides have synonym lists, so scoring one direction only would miss duplicates.
fn symmetric_assessment(
    left: &SeriesMatchFacts,
    right: &SeriesMatchFacts,
    similarity: f32,
) -> Assessment {
    let forward = one_direction(left, right, similarity);
    let backward = one_direction(right, left, similarity);
    if backward.score > forward.score {
        backward
    } else {
        forward
    }
}

fn one_direction(
    query_side: &SeriesMatchFacts,
    candidate_side: &SeriesMatchFacts,
    similarity: f32,
) -> Assessment {
    let query = Query {
        normalized_title: query_side.normalized_title.clone(),
        content_type: query_side.content_type,
        release_year: query_side.release_year,
        tags: query_side.tags.clone(),
        authors: query_side.authors.clone(),
    };
    let candidate = Candidate {
        series_id: candidate_side.series_id,
        normalized_title: candidate_side.normalized_title.clone(),
        similarity,
        alt_normalized_titles: candidate_side.alt_normalized_titles.clone(),
        content_type: candidate_side.content_type,
        release_year: candidate_side.release_year,
        tags: candidate_side.tags.clone(),
        authors: candidate_side.authors.clone(),
    };
    assess(&query, &candidate)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tankovault_domain::ContentType;

    fn facts(title: &str, alts: &[&str], sources: i64, chapters: i64) -> SeriesMatchFacts {
        SeriesMatchFacts {
            series_id: SeriesId::new(),
            canonical_title: title.to_owned(),
            normalized_title: tankovault_domain::normalize_title(title),
            alt_normalized_titles: alts
                .iter()
                .map(|t| tankovault_domain::normalize_title(t))
                .collect(),
            content_type: ContentType::Unknown,
            release_year: None,
            tags: Vec::new(),
            authors: Vec::new(),
            source_count: sources,
            chapter_count: chapters,
            watcher_count: 0,
        }
    }

    /// Scoring one direction only loses every signal that lives on the query side.
    ///
    /// `Query` has no alternative titles, so `assess(left, right)` sees the right side's
    /// synonyms and not the left's. Here the *left* series is the one whose synonym matches, and
    /// a one-directional sweep would score the pair on its canonical titles alone — which share
    /// nothing — and file two obvious duplicates as distinct.
    #[test]
    fn a_synonym_on_either_side_is_enough() {
        let left = facts("Solo Leveling", &["Na Honjaman Level Up"], 1, 10);
        let right = facts("Na Honjaman Level Up", &[], 1, 10);

        let forward = one_direction(&left, &right, 0.0);
        let backward = one_direction(&right, &left, 0.0);
        assert!(
            !forward.signals.is_structural(),
            "sanity: the canonical titles alone are not an identity match"
        );
        assert!(backward.signals.alias_identity, "{backward:?}");
        assert!(
            symmetric_assessment(&left, &right, 0.0)
                .signals
                .is_structural()
        );
    }

    /// The survivor is the series carrying more of the catalogue, in both orderings.
    ///
    /// Which id sat in `series_id` and which in `candidate_id` used to decide this, and it
    /// recorded nothing but which series a scan happened to create second — so the console's
    /// merge button routinely deleted the *older*, richer series and kept a stub.
    #[test]
    fn the_richer_series_survives_regardless_of_pair_order() {
        let rich = facts("Berserk", &[], 4, 380);
        let stub = facts("Berserk", &[], 1, 2);
        assert!(rich.weight() > stub.weight());

        // Whichever way round the pair arrives, the same series wins.
        let pick = |a: &SeriesMatchFacts, b: &SeriesMatchFacts| {
            if a.weight() >= b.weight() {
                a.series_id
            } else {
                b.series_id
            }
        };
        assert_eq!(pick(&rich, &stub), rich.series_id);
        assert_eq!(pick(&stub, &rich), rich.series_id);
    }

    /// The reported pair, scored the way the sweep scores it.
    ///
    /// `Sorry but I’m not Yuri` and `Sorry But Im Not Yuri` sat in the queue at 0.80 with no
    /// path to ever leaving it. With the corrected normalization their keys are identical, which
    /// is a structural signal, which is what `adjudicate` requires before merging anything.
    #[test]
    fn the_reported_duplicate_is_merged_automatically() {
        let a = facts("Sorry but I\u{2019}m not Yuri", &[], 1, 12);
        let b = facts("Sorry But Im Not Yuri", &[], 1, 9);
        let assessment = symmetric_assessment(&a, &b, 0.8);
        assert!(assessment.signals.exact_title, "{assessment:?}");
        assert_eq!(
            adjudicate(assessment, MatchingConfig::default().thresholds()),
            MergeVerdict::Auto
        );
    }

    /// A numbered sequel is never merged, however identical the rest of the title is.
    #[test]
    fn a_sequel_is_not_swept_up() {
        let a = facts("Kingdom of the Wind", &[], 1, 40);
        let b = facts("Kingdom of the Wind 2", &[], 1, 12);
        let assessment = symmetric_assessment(&a, &b, 0.95);
        assert!(assessment.signals.numeric_conflict, "{assessment:?}");
        assert_eq!(
            adjudicate(assessment, MatchingConfig::default().thresholds()),
            MergeVerdict::Distinct
        );
    }
}
