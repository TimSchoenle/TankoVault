//! The standing duplicate sweep: find series the catalogue holds twice, merge the certain ones
//! and queue the rest.
//!
//! # Why this exists at all
//!
//! Canonicalisation used to happen in exactly one place — `resolve_canonical_series`, while a
//! scanned source was being filed — and that place is the worst-informed moment in a series'
//! life. The row being scored has no tags, no authors, no release year and no alternative
//! titles yet; all of those arrive from a later enrichment pass, by which time the decision has
//! been taken and nothing revisits it. A merge candidate recorded then is a *floor* on the
//! score, not a verdict, and a duplicate missed then is missed forever.
//!
//! On a 26 418-series catalogue that produced 2 676 open candidates that nothing would ever
//! re-judge, and 59 pairs with byte-identical whitespace-stripped titles that had never been
//! queued at all because their trigram similarity (0.37–0.58) never reached the review floor.
//!
//! This sweep is the second look. It blocks the catalogue on the compact title key — an
//! equality, so an index lookup rather than a similarity scan — re-scores every shortlisted
//! pair with everything now known about both sides, and acts:
//!
//! - **[`MergeVerdict::Auto`]** — merge, without asking. Requires a *structural* identity signal
//!   (the two titles are the same string modulo whitespace, or one side answers to the other's
//!   name exactly) **and** a score at or above `matching.auto_merge`. A high score alone is not
//!   enough, and never will be: the pairs that reach 0.97 on fuzzy similarity alone are exactly
//!   the ones an operator needs to look at.
//! - **[`MergeVerdict::Review`]** — record or refresh a queue row.
//! - **[`MergeVerdict::Distinct`]** — withdraw an open queue row, if there is one.
//!
//! # Convergence
//!
//! A merge invalidates the facts the sweep loaded, so any further pair naming the absorbed
//! series is skipped for the rest of the run. Chains (A ≡ B ≡ C where A and C do not collide
//! directly) therefore resolve one link per sweep — and they *do* resolve, because merging B
//! into A gives A the title B was found by, so the next sweep's alias blocking finds (A, C).

use std::collections::{HashMap, HashSet};

use tankovault_config::MatchingConfig;
use tankovault_contracts::admin::MergeSweepView;
use tankovault_db::PgPool;
use tankovault_db::repo::matching::{self, DuplicatePair, SeriesMatchFacts};
use tankovault_domain::matching::MergeVerdict;
use tankovault_domain::{SeriesId, UserId};
use tankovault_matcher::{Assessment, Candidate, Query, adjudicate, assess};

/// How much work one sweep may do.
///
/// Every field is a bound on a *background* action, and the last one is a bound on a background
/// action that deletes rows. A sweep with no automatic-merge ceiling would let a mistaken
/// threshold change collapse the whole catalogue between two scheduler ticks, with nothing
/// between the mistake and the damage; with one, the blast radius of any single bad run is a
/// number an operator chose.
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
/// `actor` is the operator who triggered it, or `None` for the scheduled run — it lands in
/// `merge_candidates.resolved_by`, so an automatic merge is attributable to the schedule rather
/// than being silently unattributed.
///
/// # Errors
///
/// Propagates database failures. A failure part-way through leaves the merges already committed
/// — each is its own transaction — which is the correct granularity: they are independent
/// decisions, and rolling back a merge that was right because a later one failed would be worse
/// than stopping. The report describes what was done before the failure only when the call
/// succeeds; on error the caller should assume partial progress and re-run, which is safe
/// because every action the sweep takes is idempotent.
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

    // Two batched reads for the whole shortlist rather than two per pair. `pair_similarities`
    // is the trigram term, which is a property of the pair and not of either series — without it
    // an open queue row whose score came from a trigram match would be re-scored lower on no new
    // evidence, and withdrawn.
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
        // A series merged earlier in this run no longer exists, and the facts loaded for it are
        // stale. See the module docs on convergence.
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
                // The survivor is the series carrying more of the catalogue. Both sides' data is
                // preserved either way — the merge unions everything — but the absorbed id stops
                // existing, and every bookmark, notification and tracker mapping naming it
                // breaks.
                let (keep, drop) = if left.weight() >= right.weight() {
                    (left, right)
                } else {
                    (right, left)
                };
                matching::merge_series(
                    pool,
                    keep.series_id,
                    drop.series_id,
                    actor,
                    "auto_merged",
                )
                .await?;
                absorbed.insert(drop.series_id);
                report.auto_merged += 1;
                // Logged at info, with both titles and the deciding signals: this is a
                // destructive action taken without a human, so the log line has to be enough to
                // audit the decision after the fact.
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
/// The scorer is not symmetric: [`Query`] carries no alternative titles, so
/// `assess(left, right)` can see the *right* side's synonyms and not the left's. For a scan
/// resolving an incoming source that asymmetry is correct — the incoming source genuinely has
/// one title. Here both sides are established series with their own synonym lists, and scoring
/// one direction only would miss every duplicate whose evidence happens to sit on the left.
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
