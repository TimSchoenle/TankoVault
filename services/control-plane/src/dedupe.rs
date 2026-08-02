//! The standing duplicate sweep: finds series the catalogue holds twice, merges the
//! certain ones, and queues the rest for review, using richer signals than were available
//! when the series were first ingested.

use std::collections::{HashMap, HashSet};

use tankovault_config::MatchingConfig;
use tankovault_contracts::admin::MergeSweepView;
use tankovault_db::PgPool;
use tankovault_db::repo::matching::{self, DuplicatePair, QueueOutcome, SeriesMatchFacts};
use tankovault_domain::matching::MergeVerdict;
use tankovault_domain::{SeriesId, UserId};
use tankovault_matcher::{Assessment, Candidate, Query, adjudicate, assess};

/// How much work one sweep may do. `max_auto_merges` bounds a background action that deletes
/// rows — without it, a bad threshold change could collapse the whole catalogue in one run.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SweepBudget {
    /// Newly-blocked pairs to shortlist — ones the sweep has never recorded a verdict for.
    pub pairs: i64,
    /// Open queue rows to re-score, oldest-refreshed first.
    pub requeue: i64,
    /// Pairs previously judged distinct to reconsider, oldest-refreshed first.
    pub recheck: i64,
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
    // Three disjoint sources, because each needs its own budget: pairs never judged before,
    // pairs an operator has yet to decide, and pairs the scorer judged apart on evidence that
    // enrichment has since changed. `find_duplicate_pairs` excludes everything the other two
    // return, which is what stops the shortlist re-offering a fixed prefix every run.
    let mut pairs = matching::find_duplicate_pairs(pool, budget.pairs).await?;
    pairs.extend(matching::open_merge_pairs(pool, budget.requeue).await?);
    pairs.extend(matching::distinct_merge_pairs(pool, budget.recheck).await?);
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
                match matching::record_merge_candidate(
                    pool,
                    a,
                    b,
                    assessment.score,
                    &labels,
                    "duplicate sweep",
                )
                .await?
                {
                    QueueOutcome::Added => report.queued += 1,
                    QueueOutcome::Refreshed => report.requeued += 1,
                    QueueOutcome::Reopened => report.reopened += 1,
                    QueueOutcome::Unchanged => {}
                }
            }
            MergeVerdict::Distinct => {
                let labels = assessment.signals.labels();
                if matching::record_distinct_pair(pool, a, b, assessment.score, &labels).await? {
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

/// Every title `query_side` answers to, scored against `candidate_side`, keeping the best.
///
/// The alias loop is what makes the third of `find_duplicate_pairs`' three collisions scorable
/// at all. Two rounds of canonical-as-query cover canonical-against-canonical and
/// canonical-against-alias, but never compare *two aliases* — so a pair shortlisted purely
/// because both sides carry the same synonym reached no structural signal, `adjudicate` was
/// barred from merging it whatever it scored, and it sat in the review queue permanently.
///
/// The canonical title goes first and ties are kept, so a pair that both a canonical title and
/// an alias explain is reported with the canonical reading's signals.
fn one_direction(
    query_side: &SeriesMatchFacts,
    candidate_side: &SeriesMatchFacts,
    similarity: f32,
) -> Assessment {
    let mut query = Query {
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

    let mut best = assess(&query, &candidate);
    for alias in &query_side.alt_normalized_titles {
        query.normalized_title.clone_from(alias);
        let assessment = as_alias_match(assess(&query, &candidate));
        if assessment.score > best.score {
            best = assessment;
        }
    }
    best
}

/// Re-label an identity hit that came from one of the query side's *alternative* titles.
///
/// `exact_title` and `compact_identity` assert that the two series' **canonical** titles are the
/// same string. That claim is what the console renders as a badge and what the auto-merge log
/// records as its justification for deleting a row, and it is false when the title that matched
/// was a synonym. `alias_identity` is the honest signal and is structural just the same, so the
/// verdict does not move — only the reason given for it.
fn as_alias_match(mut assessment: Assessment) -> Assessment {
    if assessment.signals.exact_title || assessment.signals.compact_identity {
        assessment.signals.alias_identity = true;
        assessment.signals.exact_title = false;
        assessment.signals.compact_identity = false;
    }
    assessment
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

    /// A synonym identifies the pair from whichever side it sits on.
    ///
    /// `Query` has no alternative titles, so a sweep that scored the query side's canonical
    /// title alone saw only the *right* side's synonyms. Here the synonym is on the left, and
    /// the canonical titles share nothing — so that sweep filed two obvious duplicates as
    /// distinct.
    #[test]
    fn a_synonym_on_either_side_is_enough() {
        let left = facts("Solo Leveling", &["Na Honjaman Level Up"], 1, 10);
        let right = facts("Na Honjaman Level Up", &[], 1, 10);

        // Sanity: without the synonym the pair is not an identity match at all.
        let bare_left = facts("Solo Leveling", &[], 1, 10);
        assert!(
            !symmetric_assessment(&bare_left, &right, 0.0)
                .signals
                .is_structural()
        );

        assert!(one_direction(&left, &right, 0.0).signals.alias_identity);
        assert!(one_direction(&right, &left, 0.0).signals.alias_identity);
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

    /// The two KunManga listings the user reported, scored the way the sweep scores them.
    ///
    /// One series' canonical title is byte-identical to the other's alternative title, which is
    /// `alias_identity` — structural, so the merge is permitted at all — and the year is within
    /// a year, so the score reaches the ceiling. Nothing about the *scoring* was ever wrong here:
    /// the pair sat split for a day because `find_duplicate_pairs` never reached it. This pins
    /// the half that has to stay true for that fix to have any effect.
    #[test]
    fn the_reported_pair_merges_once_the_sweep_can_reach_it() {
        let long = "The Capital\u{2019}s One-Man Army Golem Master\u{2014}Unexpectedly Banished!? \
                    ~Now That I\u{2019}m Free, I\u{2019}ll Create the Ultimate Golem Together \
                    With My Beautiful Hero Disciples. I Don\u{2019}t Care Even if They Beg Me to \
                    Come Back~";
        let short = "I'll Create the Ultimate Golem Together with My Beautiful Hero Disciples. \
                     I Don't Care Even If They Beg Me to Come Back";

        let mut subtitled = facts(long, &[short], 1, 18);
        subtitled.release_year = Some(2025);
        let mut bare = facts(short, &[], 1, 18);
        bare.release_year = Some(2026);

        let assessment = symmetric_assessment(&subtitled, &bare, 0.4);
        assert!(assessment.signals.alias_identity, "{assessment:?}");
        assert_eq!(
            adjudicate(assessment, MatchingConfig::default().thresholds()),
            MergeVerdict::Auto
        );
    }

    /// Two series that agree on nothing but a *shared synonym* are still a duplicate.
    ///
    /// `find_duplicate_pairs` shortlists this collision — alias against alias, the same work
    /// listed under its romaji name by two providers that each chose a different english title —
    /// but the scorer could not see it: `Query` has no alternative titles, so both directions
    /// compared a *canonical* title against the other side's names and the one title the two
    /// series actually share was never an input. No structural signal meant `adjudicate` refused
    /// to merge at any score, so 438 such pairs sat in the review queue with no way out of it.
    #[test]
    fn a_synonym_shared_by_both_sides_is_still_an_identity_match() {
        let romaji = "yuusha party wo tsuihou sareta beast tamer";
        let left = facts("The Beast Tamer Exiled From His Party", &[romaji], 1, 30);
        let right = facts("Banished Monster Tamer", &[romaji], 1, 24);

        // Sanity: strip the synonym and nothing identifies the pair, so the assertion below
        // cannot be passing on the canonical titles.
        let bare_left = facts("The Beast Tamer Exiled From His Party", &[], 1, 30);
        let bare_right = facts("Banished Monster Tamer", &[], 1, 24);
        assert!(
            !symmetric_assessment(&bare_left, &bare_right, 0.0)
                .signals
                .is_structural()
        );

        let assessment = symmetric_assessment(&left, &right, 0.0);
        assert!(assessment.signals.alias_identity, "{assessment:?}");
        assert!(
            !assessment.signals.exact_title && !assessment.signals.compact_identity,
            "a synonym hit must not claim the canonical titles are the same string: {assessment:?}"
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
