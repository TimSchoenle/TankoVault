//! Owns one pass over the shortlist: read each pair, adjudicate it, act on the verdict, and queue
//! the rest.

use std::collections::{HashMap, HashSet};

use tankovault_contracts::admin::MergeSweepView;
use tankovault_db::PgPool;
use tankovault_db::repo::matching::{
    self, DistinctVerdict, DuplicatePair, MergeUndo, NewMergeCandidate, NewMergeDecision,
    QueueOutcome, SeriesMatchFacts,
};
use tankovault_domain::matching::{LiveTiming, MergeVerdict};
use tankovault_domain::{SeriesId, UserId};
use tankovault_matcher::{
    Adjudication, PairReading, PairSide, Thresholds, adjudicate, apply_live_timing, read_pair,
    wants_live_timing,
};
use uuid::Uuid;

use super::{
    journal::{JournalContext, journal, journal_payloads, policy_json, review_reason},
    judgement::{Action, Judged, Trigger},
    run::{SweepBudget, SweepScope},
};

/// Runs one pass of a sweep.
///
/// # Three phases, and why the merges are not in the last one
///
/// Judging is cheap and writing is not: a run judges thousands of pairs, and two statements on each
/// would make round trips (not scoring, and not the shortlist query) the cost of a sweep. The
/// review and distinct verdicts and their journal rows are therefore buffered and written once
/// each.
///
/// Merges stay inline in the judging phase, for two independent reasons. They mutate the
/// catalogue, so every later pair naming an absorbed series has stale facts and must be skipped;
/// and a merge's journal row is the only one carrying an undo record, so batching it would put
/// the ability to revert an entire run's merges behind a single best-effort statement.
///
/// # Errors
///
/// Returns an error on a database failure; merges already committed stay committed (idempotent,
/// safe to re-run).
pub(super) async fn sweep_once(
    pool: &PgPool,
    policy: Thresholds,
    budget: SweepBudget,
    scope: SweepScope,
    actor: Option<UserId>,
) -> anyhow::Result<(MergeSweepView, Vec<DuplicatePair>)> {
    let (pairs, triggers) = shortlist(pool, budget, scope).await?;
    if pairs.is_empty() {
        return Ok((MergeSweepView::default(), Vec::new()));
    }

    let pass = Pass {
        pool,
        thresholds: policy,
        budget,
        triggers,
        ctx: JournalContext {
            // One id for the whole run, so a bad sweep can be read, and reverted, as a unit.
            sweep_id: Uuid::now_v7(),
            actor,
            policy: policy_json(policy),
        },
        // Batched reads for the whole shortlist. `pair_similarities` is per-pair, not
        // per-series; omitting it would re-score a trigram-matched row lower on no new evidence.
        facts: load_facts(pool, &pairs).await?,
        similarity: matching::pair_similarities(pool, &pairs)
            .await?
            .into_iter()
            .collect(),
    };

    let mut report = MergeSweepView::default();
    let judged = pass.judge(&pairs, &mut report).await?;
    let writes = flush_queue_writes(pool, &judged, &mut report).await?;
    let repeat_deferrals = repeat_deferrals(pool, &judged).await;

    let payloads = journal_payloads(&judged, &writes, &repeat_deferrals);
    let decisions: Vec<NewMergeDecision<'_>> = payloads
        .iter()
        .map(|(judged, payload)| judged.journal(&pass.ctx, payload, None))
        .collect();
    journal(pool, &decisions).await;

    Ok((report, pairs))
}

/// Everything one pass needs that does not change while it runs.
struct Pass<'a> {
    pool: &'a PgPool,
    thresholds: Thresholds,
    budget: SweepBudget,
    triggers: HashMap<DuplicatePair, Trigger>,
    ctx: JournalContext,
    facts: HashMap<SeriesId, SeriesMatchFacts>,
    similarity: HashMap<DuplicatePair, f32>,
}

impl Pass<'_> {
    /// Score every shortlisted pair, and perform the merges as they are decided.
    ///
    /// # Errors
    ///
    /// A failed merge. Everything else this pass decides is written by the caller, in batches.
    async fn judge(
        &self,
        pairs: &[DuplicatePair],
        report: &mut MergeSweepView,
    ) -> anyhow::Result<Vec<Judged<'_>>> {
        let mut absorbed: HashSet<SeriesId> = HashSet::new();
        let mut judged: Vec<Judged<'_>> = Vec::with_capacity(pairs.len());

        for &pair in pairs {
            let (a, b) = pair;
            // Facts loaded for an already-merged series are stale; skip it for the rest of the
            // pass. `sweep` runs another instead of leaving the chain for the next tick.
            if absorbed.contains(&a) || absorbed.contains(&b) {
                report.chains_deferred += 1;
                continue;
            }
            let (Some(left), Some(right)) = (self.facts.get(&a), self.facts.get(&b)) else {
                continue;
            };

            let trigram = self.similarity.get(&pair).copied().unwrap_or_default();
            let mut reading = best_reading(left, right, trigram);
            let mut verdict = adjudicate(reading.assessment, self.thresholds);
            let live_timing = if wants_live_timing(&reading, &verdict) {
                let timing =
                    matching::live_timing(self.pool, left.series_id, right.series_id).await?;
                apply_live_timing(&mut reading, timing);
                verdict = adjudicate(reading.assessment, self.thresholds);
                Some(timing)
            } else {
                None
            };
            let signals = reading.assessment.signals.labels();
            let merge = matches!(verdict.verdict, MergeVerdict::Auto)
                && report.auto_merged < self.budget.max_auto_merges;

            let (action, reason) = match verdict.verdict {
                MergeVerdict::Auto if !merge => (Action::Deferred, None),
                MergeVerdict::Auto => {
                    // Survivor keeps more of the catalogue. The absorbed id stops existing,
                    // breaking any bookmark, notification or tracker mapping naming it directly.
                    let (keep, drop) = if left.weight() >= right.weight() {
                        (left, right)
                    } else {
                        (right, left)
                    };
                    let undo = self.merge(keep, drop, &reading, &verdict, &signals).await?;
                    absorbed.insert(drop.series_id);
                    report.auto_merged += 1;

                    let entry = self.judgement(
                        pair,
                        left,
                        right,
                        trigram,
                        reading,
                        live_timing,
                        verdict,
                        signals,
                        None,
                        Action::Merged { keep, drop },
                    );
                    // Written immediately, alone: this row carries the undo record, and it is the
                    // only thing standing between a wrong automatic merge and an unrecoverable one.
                    let payload = entry.payload("merged");
                    journal(
                        self.pool,
                        &[entry.journal(&self.ctx, &payload, Some(&undo))],
                    )
                    .await;
                    judged.push(entry);
                    continue;
                }
                MergeVerdict::Review => (Action::Review, Some(review_reason(&verdict))),
                MergeVerdict::Distinct => (Action::Distinct, None),
            };

            judged.push(self.judgement(
                pair,
                left,
                right,
                trigram,
                reading,
                live_timing,
                verdict,
                signals,
                reason,
                action,
            ));
        }

        report.chains_deferred += retract_absorbed(&mut judged, &absorbed);
        count_judgements(&judged, report);
        Ok(judged)
    }

    /// Merges `drop` into `keep`, logging the decision that deleted a row.
    async fn merge(
        &self,
        keep: &SeriesMatchFacts,
        drop: &SeriesMatchFacts,
        reading: &PairReading,
        verdict: &Adjudication,
        signals: &[&str],
    ) -> anyhow::Result<MergeUndo> {
        let undo = matching::merge_series(
            self.pool,
            keep.series_id,
            drop.series_id,
            self.ctx.actor,
            "auto_merged",
        )
        .await?;
        // Info-level with both titles and signals: this is a destructive, human-free action, so
        // the log line must be enough to audit the decision after the fact even for an operator
        // who never opens the console.
        tracing::info!(
            keep = %keep.series_id,
            keep_title = %keep.canonical_title,
            dropped = %drop.series_id,
            dropped_title = %drop.canonical_title,
            score = reading.assessment.score,
            reason = verdict.reason,
            signals = ?signals,
            matched_title = %reading.matched_candidate_title,
            undo_rows = undo.row_count(),
            "auto-merged duplicate series"
        );
        Ok(undo)
    }

    /// Assemble one judgement, stamping it with the shortlist it came from.
    #[expect(
        clippy::too_many_arguments,
        reason = "the fields of `Judged`, which exists precisely so the action taken and the \
                  record of that action are read out of one value. Grouping them into a second \
                  struct to pass them here would be that struct twice"
    )]
    fn judgement<'f>(
        &self,
        pair: DuplicatePair,
        left: &'f SeriesMatchFacts,
        right: &'f SeriesMatchFacts,
        trigram: f32,
        reading: PairReading,
        live_timing: Option<LiveTiming>,
        verdict: Adjudication,
        signals: Vec<&'static str>,
        reason: Option<String>,
        action: Action<'f>,
    ) -> Judged<'f> {
        Judged {
            pair,
            trigger: self.triggers.get(&pair).copied().unwrap_or(Trigger::New),
            left,
            right,
            trigram,
            reading,
            live_timing,
            verdict,
            signals,
            reason,
            action,
        }
    }
}

/// Drops the judgements this pass invalidated by merging one of their series away, returning how
/// many: a chain deferral, exactly like a pair the judging loop skipped after the merge.
///
/// That loop can only skip pairs it has **not yet reached**. A pair judged *before* the merge kept
/// a verdict naming a series the merge then deleted, and those verdicts are written in one batch
/// after the loop against two columns that are foreign keys into `series`: the row was rejected,
/// the whole statement with it, and the sweep failed with
/// `merge_candidates_candidate_id_fkey`. Any future rework that moves the merges out of the
/// judging loop, or the queue writes into it, has to keep this ordering fact answered somewhere.
///
/// Merges themselves are exempt: their pair names the absorbed series by definition, and their
/// journal row is already written and carries no foreign key.
pub(super) fn retract_absorbed(judged: &mut Vec<Judged<'_>>, absorbed: &HashSet<SeriesId>) -> i64 {
    if absorbed.is_empty() {
        return 0;
    }
    let mut retracted = 0_i64;
    judged.retain(|entry| {
        let stale = !matches!(entry.action, Action::Merged { .. })
            && (absorbed.contains(&entry.pair.0) || absorbed.contains(&entry.pair.1));
        retracted += i64::from(stale);
        !stale
    });
    retracted
}

/// Fold what the pass decided into its report.
///
/// Counted after [`retract_absorbed`], not as each pair is judged: a judgement the pass went on to
/// invalidate was not one it examined to a conclusion, and counting it there would leave
/// `pairs_examined` and `blocked` describing verdicts nothing acted on. `auto_merged` is counted
/// in the loop instead, because the merge budget is spent against it as it goes.
fn count_judgements(judged: &[Judged<'_>], report: &mut MergeSweepView) {
    for entry in judged {
        report.pairs_examined += 1;
        if !entry.verdict.blocked_by.is_empty() {
            report.blocked += 1;
        }
        if matches!(entry.action, Action::Deferred) {
            report.deferred += 1;
        }
    }
}

/// The pairs this sweep will judge, and which shortlist each came from.
///
/// Three disjoint sources, because each needs its own budget: pairs never judged before, pairs
/// an operator has yet to decide, and pairs the scorer judged apart on evidence that enrichment
/// has since changed. `find_duplicate_pairs` excludes everything the other two return, which is
/// what stops the shortlist re-offering a fixed prefix every run.
pub(super) async fn shortlist(
    pool: &PgPool,
    budget: SweepBudget,
    scope: SweepScope,
) -> anyhow::Result<(Vec<DuplicatePair>, HashMap<DuplicatePair, Trigger>)> {
    let discovered = match scope {
        SweepScope::Full => matching::find_duplicate_pairs(pool, budget.pairs).await?,
        SweepScope::Rotation => Vec::new(),
    };
    let mut triggers: HashMap<DuplicatePair, Trigger> = HashMap::new();
    for (pairs, trigger) in [
        (discovered, Trigger::New),
        (
            matching::open_merge_pairs(pool, budget.requeue).await?,
            Trigger::Requeue,
        ),
        (
            matching::distinct_merge_pairs(pool, budget.recheck).await?,
            Trigger::Recheck,
        ),
    ] {
        for pair in pairs {
            triggers.entry(pair).or_insert(trigger);
        }
    }
    let mut pairs: Vec<DuplicatePair> = triggers.keys().copied().collect();
    pairs.sort_unstable();
    Ok((pairs, triggers))
}

/// What the batched queue upserts did, keyed by pair.
pub(super) enum QueueWrite {
    Reviewed(QueueOutcome),
    /// Whether the pair had an open row that this verdict closed.
    Judged {
        was_open: bool,
    },
}

/// Writes the review and distinct verdicts, one statement each, and counts them into `report`.
async fn flush_queue_writes(
    pool: &PgPool,
    judged: &[Judged<'_>],
    report: &mut MergeSweepView,
) -> anyhow::Result<HashMap<DuplicatePair, QueueWrite>> {
    let reviews: Vec<NewMergeCandidate<'_>> = judged
        .iter()
        .filter(|j| matches!(j.action, Action::Review))
        .map(|j| NewMergeCandidate {
            pair: j.pair,
            score: j.reading.assessment.score,
            signals: &j.signals,
            // Every `Action::Review` is built with its sentence in the same expression.
            reason: j.reason.as_deref().unwrap_or_default(),
        })
        .collect();
    let distinct: Vec<DistinctVerdict<'_>> = judged
        .iter()
        .filter(|j| matches!(j.action, Action::Distinct))
        .map(|j| DistinctVerdict {
            pair: j.pair,
            score: j.reading.assessment.score,
            signals: &j.signals,
        })
        .collect();

    let mut writes = HashMap::with_capacity(reviews.len() + distinct.len());
    for (a, b, outcome) in matching::record_merge_candidates(pool, &reviews).await? {
        match outcome {
            QueueOutcome::Added => report.queued += 1,
            QueueOutcome::Refreshed => report.requeued += 1,
            QueueOutcome::Reopened => report.reopened += 1,
            QueueOutcome::Unchanged => {}
        }
        writes.insert((a, b), QueueWrite::Reviewed(outcome));
    }
    for (pair, was_open) in matching::record_distinct_pairs(pool, &distinct).await? {
        if was_open {
            report.withdrawn += 1;
        } else {
            report.distinct += 1;
        }
        writes.insert(pair, QueueWrite::Judged { was_open });
    }
    Ok(writes)
}

/// The deferred pairs of this pass whose latest journal row is already a deferral.
///
/// Best-effort like [`journal`]: a failed lookup journals every deferral, which is the old
/// behaviour, instead of failing a pass whose merges are already committed.
pub(super) async fn repeat_deferrals(
    pool: &PgPool,
    judged: &[Judged<'_>],
) -> HashSet<DuplicatePair> {
    let deferred: Vec<DuplicatePair> = judged
        .iter()
        .filter(|j| matches!(j.action, Action::Deferred))
        .map(|j| j.pair)
        .collect();
    match matching::pairs_already_deferred(pool, &deferred).await {
        Ok(pairs) => pairs.into_iter().collect(),
        Err(e) => {
            tracing::warn!(error = %e, "could not read prior deferrals; journalling them all");
            HashSet::new()
        }
    }
}

/// Loads [`SeriesMatchFacts`] for every series named by `pairs`, keyed by id.
pub(super) async fn load_facts(
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

/// One side of a pair, as the matcher reads it.
pub(super) fn side(facts: &SeriesMatchFacts) -> PairSide<'_> {
    PairSide {
        series_id: facts.series_id,
        normalized_title: &facts.normalized_title,
        alt_normalized_titles: &facts.alt_normalized_titles,
        source_titles: &facts.source_titles,
        anilist_id: facts.anilist_id.as_deref(),
        content_type: facts.content_type,
        release_year: facts.release_year,
        tags: &facts.tags,
        authors: &facts.authors,
    }
}

/// The pair's winning reading; see [`tankovault_matcher::read_pair`].
pub(super) fn best_reading(
    left: &SeriesMatchFacts,
    right: &SeriesMatchFacts,
    trigram: f32,
) -> PairReading {
    read_pair(side(left), side(right), trigram)
}
