//! The standing duplicate sweep: finds series the catalogue holds twice, merges the
//! certain ones, and queues the rest for review, using richer signals than were available
//! when the series were first ingested.
//!
//! Every pair the sweep acts on is journalled in `merge_decisions` with the itemised score, the
//! rule that decided it, the guards that held it back and — for a merge — the undo record that
//! takes it back. See `Decision::record`.

use std::collections::{HashMap, HashSet};

use serde_json::json;
use tankovault_config::MatchingConfig;
use tankovault_contracts::admin::MergeSweepView;
use tankovault_db::PgPool;
use tankovault_db::repo::matching::{
    self, DistinctVerdict, DuplicatePair, MergeUndo, NewMergeCandidate, NewMergeDecision,
    QueueOutcome, SeriesMatchFacts,
};
use tankovault_domain::matching::MergeVerdict;
use tankovault_domain::{SeriesId, UserId};
use tankovault_matcher::{
    Adjudication, Candidate, Explanation, Query, Thresholds, adjudicate, assess, explain,
};
use uuid::Uuid;

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

/// Which shortlists one sweep draws from.
///
/// Discovery blocks the whole catalogue on the compact title key, so it costs the same whether
/// anything changed or not; the two rotations are index scans sized by their own budgets.
/// Separating them is what lets the queue be re-scored on a cadence discovery could not afford —
/// see `scheduler.merge_sweep_rotation_interval_secs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SweepScope {
    /// All three shortlists, including pairs never judged before.
    Full,
    /// The requeue and recheck rotations only.
    Rotation,
}

/// Which shortlist a pair came from, recorded on its decision so a run can be read back by the
/// question it was answering rather than as one undifferentiated list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Trigger {
    New,
    Requeue,
    Recheck,
}

impl Trigger {
    const fn as_str(self) -> &'static str {
        match self {
            Self::New => "sweep_new",
            Self::Requeue => "sweep_requeue",
            Self::Recheck => "sweep_recheck",
        }
    }
}

/// What the judging pass did about a pair, and therefore what the journal has to record.
#[derive(Debug, Clone, Copy)]
enum Action<'a> {
    /// Merged. Journalled on the spot rather than with the batch, because this is the only
    /// outcome whose record carries an undo — see [`sweep`].
    Merged {
        keep: &'a SeriesMatchFacts,
        drop: &'a SeriesMatchFacts,
    },
    /// The automatic-merge budget was spent before this pair's turn came.
    Deferred,
    /// Offered to an operator. What that did to the queue comes from the batched upsert.
    Review,
    /// Judged apart. Whether it closed an open row comes from the batched upsert.
    Distinct,
}

/// One pair, judged.
///
/// Carries everything the queue writes *and* the journal need, so the action taken and the record
/// of that action are read out of one value rather than rebuilt independently in two places.
struct Judged<'a> {
    pair: DuplicatePair,
    trigger: Trigger,
    left: &'a SeriesMatchFacts,
    right: &'a SeriesMatchFacts,
    trigram: f32,
    reading: PairReading,
    verdict: Adjudication,
    signals: Vec<&'static str>,
    /// The console sentence, for the pairs going to review.
    reason: Option<String>,
    action: Action<'a>,
}

/// The parts of a journal row that are the same for every decision in a run.
struct JournalContext {
    sweep_id: Uuid,
    actor: Option<UserId>,
    policy: serde_json::Value,
}

/// The parts of a journal row derived from what actually happened to a pair.
struct JournalPayload {
    outcome: &'static str,
    terms: serde_json::Value,
    evidence: serde_json::Value,
}

impl Judged<'_> {
    /// The journal row for this judgement.
    fn journal<'b>(
        &'b self,
        ctx: &'b JournalContext,
        payload: &'b JournalPayload,
        undo: Option<&'b MergeUndo>,
    ) -> NewMergeDecision<'b> {
        let (survivor, absorbed) = match self.action {
            Action::Merged { keep, drop } => (Some(keep.series_id), Some(drop.series_id)),
            _ => (None, None),
        };
        NewMergeDecision {
            sweep_id: Some(ctx.sweep_id),
            trigger: self.trigger.as_str(),
            actor: ctx.actor,
            pair: (self.left.series_id, self.right.series_id),
            titles: (&self.left.canonical_title, &self.right.canonical_title),
            verdict: match self.verdict.verdict {
                MergeVerdict::Auto => "auto",
                MergeVerdict::Review => "review",
                MergeVerdict::Distinct => "distinct",
            },
            reason: self.verdict.reason,
            blocked_by: &self.verdict.blocked_by,
            outcome: payload.outcome,
            survivor_id: survivor,
            absorbed_id: absorbed,
            score: self.reading.assessment.score,
            base_score: self.reading.base,
            signals: &self.signals,
            terms: &payload.terms,
            evidence: &payload.evidence,
            policy: &ctx.policy,
            undo,
        }
    }

    /// The itemised score and both sides' facts, as the journal stores them.
    fn payload(&self, outcome: &'static str) -> JournalPayload {
        let merged = match self.action {
            Action::Merged { keep, drop } => Some((keep, drop)),
            _ => None,
        };
        JournalPayload {
            outcome,
            terms: terms_json(&self.reading),
            evidence: evidence_json(self.left, self.right, self.trigram, &self.reading, merged),
        }
    }
}

/// The most passes one sweep will make.
///
/// A pass beyond the first runs only when the one before it skipped a pair because of its own
/// merge, so this bounds the work a pathological chain can ask for. It does **not** bound the
/// damage: every pass is charged against the same [`SweepBudget::max_auto_merges`], so a sweep
/// deletes no more rows for having taken three passes than it would have in one.
const MAX_PASSES: u32 = 3;

/// Run a sweep, following the merge chains it creates.
///
/// `actor` is the operator who triggered it, or `None` for the scheduled run — recorded
/// in `merge_candidates.resolved_by` and on every decision this run journals.
///
/// # Why more than one pass
///
/// A pass that merges A into B has invalidated the facts it loaded for every other pair naming
/// A, so it skips them (`chains_deferred`). Left there, a three-way duplicate needs three
/// scheduler ticks — an hour apart each — to collapse, and for the whole of that time the
/// catalogue holds a duplicate the sweep has already proved. Re-running immediately costs
/// another shortlist query and resolves it now.
///
/// # Errors
///
/// Database failures; merges already committed stay committed (idempotent, safe to re-run).
pub(crate) async fn sweep(
    pool: &PgPool,
    policy: &MatchingConfig,
    budget: SweepBudget,
    scope: SweepScope,
    actor: Option<UserId>,
) -> anyhow::Result<MergeSweepView> {
    let mut remaining = budget;
    let mut total = MergeSweepView::default();
    for pass in 0..MAX_PASSES {
        let report = sweep_once(pool, policy, remaining, scope, actor).await?;
        accumulate(&mut total, &report);
        // Spent against the run, not the pass: the ceiling is what stands between a bad
        // threshold and a collapsed catalogue, and it has to mean the same thing however many
        // passes the chains ask for.
        remaining.max_auto_merges -= report.auto_merged;
        if report.chains_deferred == 0 || remaining.max_auto_merges <= 0 {
            break;
        }
        tracing::debug!(
            pass = pass + 1,
            chains = report.chains_deferred,
            "re-running the sweep for a merge chain"
        );
    }
    Ok(total)
}

/// Add one pass's counts into the run's totals.
///
/// Everything sums except `chains_deferred`, which is a statement about what is *left* to do and
/// so belongs to the last pass alone — summing it would report chains that later passes resolved.
fn accumulate(total: &mut MergeSweepView, pass: &MergeSweepView) {
    total.pairs_examined += pass.pairs_examined;
    total.auto_merged += pass.auto_merged;
    total.queued += pass.queued;
    total.requeued += pass.requeued;
    total.reopened += pass.reopened;
    total.withdrawn += pass.withdrawn;
    total.distinct += pass.distinct;
    total.deferred += pass.deferred;
    total.blocked += pass.blocked;
    total.chains_deferred = pass.chains_deferred;
}

/// Run one pass of a sweep.
///
/// # Three phases, and why the merges are not in the last one
///
/// Judging is cheap and writing is not: a run judges thousands of pairs and used to spend two
/// statements on each, which made round trips — not scoring, and not the shortlist query — the
/// cost of a sweep. The review and distinct verdicts and their journal rows are therefore
/// buffered and written once each.
///
/// Merges stay inline in the judging phase, for two independent reasons. They mutate the
/// catalogue, so every later pair naming an absorbed series has stale facts and must be skipped;
/// and a merge's journal row is the only one carrying an undo record, so batching it would put
/// the ability to revert an entire run's merges behind a single best-effort statement.
///
/// # Errors
///
/// Database failures; merges already committed stay committed (idempotent, safe to re-run).
async fn sweep_once(
    pool: &PgPool,
    policy: &MatchingConfig,
    budget: SweepBudget,
    scope: SweepScope,
    actor: Option<UserId>,
) -> anyhow::Result<MergeSweepView> {
    let (pairs, triggers) = shortlist(pool, budget, scope).await?;
    if pairs.is_empty() {
        return Ok(MergeSweepView::default());
    }

    let thresholds = policy.thresholds();
    let pass = Pass {
        pool,
        thresholds,
        budget,
        triggers,
        ctx: JournalContext {
            // One id for the whole run, so a bad sweep can be read — and reverted — as a unit.
            sweep_id: Uuid::now_v7(),
            actor,
            policy: policy_json(thresholds),
        },
        // Batched reads for the whole shortlist. `pair_similarities` is per-pair, not
        // per-series — omitting it would re-score a trigram-matched row lower on no new evidence.
        facts: load_facts(pool, &pairs).await?,
        similarity: matching::pair_similarities(pool, &pairs)
            .await?
            .into_iter()
            .collect(),
    };

    let mut report = MergeSweepView::default();
    let judged = pass.judge(&pairs, &mut report).await?;
    let writes = flush_queue_writes(pool, &judged, &mut report).await?;

    let payloads = journal_payloads(&judged, &writes);
    let decisions: Vec<NewMergeDecision<'_>> = payloads
        .iter()
        .map(|(judged, payload)| judged.journal(&pass.ctx, payload, None))
        .collect();
    journal(pool, &decisions).await;

    Ok(report)
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
            // pass. `sweep` runs another rather than leaving the chain for the next tick.
            if absorbed.contains(&a) || absorbed.contains(&b) {
                report.chains_deferred += 1;
                continue;
            }
            let (Some(left), Some(right)) = (self.facts.get(&a), self.facts.get(&b)) else {
                continue;
            };
            report.pairs_examined += 1;

            let trigram = self.similarity.get(&pair).copied().unwrap_or_default();
            let reading = best_reading(left, right, trigram);
            let verdict = adjudicate(reading.assessment, self.thresholds);
            if !verdict.blocked_by.is_empty() {
                report.blocked += 1;
            }
            let signals = reading.assessment.signals.labels();
            let merge = matches!(verdict.verdict, MergeVerdict::Auto)
                && report.auto_merged < self.budget.max_auto_merges;

            let (action, reason) = match verdict.verdict {
                MergeVerdict::Auto if !merge => {
                    report.deferred += 1;
                    (Action::Deferred, None)
                }
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
                        verdict,
                        signals,
                        None,
                        Action::Merged { keep, drop },
                    );
                    // Written now, alone: this row carries the undo record, and it is the only
                    // thing standing between a wrong automatic merge and an unrecoverable one.
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
                pair, left, right, trigram, reading, verdict, signals, reason, action,
            ));
        }
        Ok(judged)
    }

    /// Merge `drop` into `keep`, logging the decision that deleted a row.
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
            verdict,
            signals,
            reason,
            action,
        }
    }
}

/// The pairs this sweep will judge, and which shortlist each came from.
///
/// Three disjoint sources, because each needs its own budget: pairs never judged before, pairs
/// an operator has yet to decide, and pairs the scorer judged apart on evidence that enrichment
/// has since changed. `find_duplicate_pairs` excludes everything the other two return, which is
/// what stops the shortlist re-offering a fixed prefix every run.
async fn shortlist(
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
enum QueueWrite {
    Reviewed(QueueOutcome),
    /// Whether the pair had an open row that this verdict closed.
    Judged {
        was_open: bool,
    },
}

/// Write the review and distinct verdicts, one statement each, and count them into `report`.
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

/// The journal payloads for everything the batched writes decided, each with the judgement it
/// belongs to.
///
/// Merges are absent: they were journalled as they happened. What is left out entirely is the
/// noise — a row re-scored to the same place every hour is not a decision, and re-confirming that
/// two unrelated series are unrelated on every run would bury the decisions that matter under
/// hundreds of rows a day that say nothing changed.
///
/// The payloads are returned rather than turned into [`NewMergeDecision`]s here because those
/// borrow their payload, so it has to outlive the batch that names it.
fn journal_payloads<'a>(
    judged: &'a [Judged<'a>],
    writes: &HashMap<DuplicatePair, QueueWrite>,
) -> Vec<(&'a Judged<'a>, JournalPayload)> {
    judged
        .iter()
        .filter_map(|j| journal_outcome(j, writes).map(|outcome| (j, j.payload(outcome))))
        .collect()
}

/// The outcome to journal for one judgement, or `None` when it is not worth a row.
fn journal_outcome(
    judged: &Judged<'_>,
    writes: &HashMap<DuplicatePair, QueueWrite>,
) -> Option<&'static str> {
    match judged.action {
        // Journalled inline, at the moment of the merge.
        Action::Merged { .. } => None,
        Action::Deferred => Some("deferred"),
        Action::Review => match writes.get(&judged.pair) {
            Some(QueueWrite::Reviewed(QueueOutcome::Added)) => Some("queued"),
            Some(QueueWrite::Reviewed(QueueOutcome::Reopened)) => Some("reopened"),
            // Only a guard makes a refresh worth journalling, because that is the near-miss an
            // operator is looking for.
            Some(QueueWrite::Reviewed(QueueOutcome::Refreshed)) => {
                (!judged.verdict.blocked_by.is_empty()).then_some("requeued")
            }
            _ => None,
        },
        Action::Distinct => match writes.get(&judged.pair) {
            Some(QueueWrite::Judged { was_open: true }) => Some("withdrawn"),
            _ => None,
        },
    }
}

/// Write a batch of journal rows.
///
/// Best-effort, like the sync history it sits beside: the record of a merge must not be able to
/// fail the merge. Batching moves what a failure costs — one statement now carries a run's
/// review and distinct decisions rather than one apiece — which is why the merges, the only rows
/// carrying an undo, are written separately as they happen.
async fn journal(pool: &PgPool, decisions: &[NewMergeDecision<'_>]) {
    if decisions.is_empty() {
        return;
    }
    if let Err(e) = matching::record_merge_decisions(pool, decisions).await {
        tracing::warn!(
            error = %e,
            decisions = decisions.len(),
            "could not journal merge decisions",
        );
    }
}

/// The human sentence the review queue shows, naming the rule rather than the sweep.
fn review_reason(verdict: &Adjudication) -> String {
    match verdict.reason {
        "no_structural_identity" => {
            "duplicate sweep: titles are similar but not the same string".to_owned()
        }
        "below_auto_merge_threshold" => {
            "duplicate sweep: identical titles, below the automatic-merge threshold".to_owned()
        }
        guard => format!("duplicate sweep: would have merged automatically, held back by {guard}"),
    }
}

/// The scorer's terms, as the decision journal stores them.
fn terms_json(reading: &PairReading) -> serde_json::Value {
    serde_json::Value::Array(
        reading
            .terms
            .iter()
            .map(|t| json!({ "rule": t.rule, "delta": t.delta, "detail": t.detail }))
            .collect(),
    )
}

/// Both sides' facts, which title matched, and how the survivor was chosen.
fn evidence_json(
    left: &SeriesMatchFacts,
    right: &SeriesMatchFacts,
    trigram: f32,
    reading: &PairReading,
    merged: Option<(&SeriesMatchFacts, &SeriesMatchFacts)>,
) -> serde_json::Value {
    let side = |f: &SeriesMatchFacts| {
        json!({
            "id": f.series_id,
            "title": f.canonical_title,
            "normalized_title": f.normalized_title,
            "alt_normalized_titles": f.alt_normalized_titles,
            "content_type": f.content_type.as_str(),
            "release_year": f.release_year,
            "tags": f.tags,
            "authors": f.authors,
            "sources": f.source_count,
            "chapters": f.chapter_count,
            "watchers": f.watcher_count,
        })
    };
    json!({
        "trigram_similarity": trigram,
        "scored_from": if reading.query_is_left { "left" } else { "right" },
        "matched_query_title": reading.matched_query_title,
        "matched_candidate_title": reading.matched_candidate_title,
        "via_alias": reading.via_alias,
        "left": side(left),
        "right": side(right),
        // Why *this* id survived. The absorbed id stops existing, so the weights that chose it
        // are the part of the decision a reader with a broken bookmark will come looking for.
        "survivor_choice": merged.map(|(keep, drop)| json!({
            "kept": keep.series_id,
            "absorbed": drop.series_id,
            "kept_weight": { "sources": keep.source_count, "chapters": keep.chapter_count,
                             "watchers": keep.watcher_count },
            "absorbed_weight": { "sources": drop.source_count, "chapters": drop.chapter_count,
                                 "watchers": drop.watcher_count },
        })),
    })
}

/// The thresholds and guards in force, so a decision can be re-judged after they change.
fn policy_json(thresholds: Thresholds) -> serde_json::Value {
    json!({
        "high": thresholds.high,
        "low": thresholds.low,
        "auto_merge": thresholds.auto_merge,
        "guards": {
            "numeric_conflict": thresholds.guards.numeric_conflict,
            "author_conflict": thresholds.guards.author_conflict,
            "year_conflict": thresholds.guards.year_conflict,
            "type_conflict": thresholds.guards.type_conflict,
        },
    })
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

/// The winning reading of a pair: which title on which side was scored against which, the
/// resulting assessment, and the itemised terms behind it.
struct PairReading {
    assessment: tankovault_matcher::Assessment,
    base: f32,
    terms: Vec<tankovault_matcher::ScoreTerm>,
    matched_query_title: String,
    matched_candidate_title: String,
    /// Whether the *candidate* side's matching title was an alternative.
    via_alias: bool,
    /// Whether the query side of the winning reading was the left series.
    query_is_left: bool,
}

/// Score the pair every way round and keep the best reading, explained.
///
/// # Why both directions, and every alias
///
/// `Query` carries no alternative titles, so `assess(left, right)` sees only the right side's
/// synonyms. Correct for a scan — the incoming source genuinely has one title — but here both
/// sides have synonym lists, and scoring one direction only misses the duplicate whose shared
/// name sits on the *query* side. Two rounds of canonical-as-query cover canonical-against-
/// canonical and canonical-against-alias but never compare two aliases, which is the third of
/// `find_duplicate_pairs`' three collisions: 438 such pairs sat permanently in the review queue
/// because no structural signal could fire for them.
///
/// # Why so little is built inside the loop
///
/// A pair with `k` aliases a side is `2(k+1)` readings, and the only thing that varies between
/// readings of one direction is the query's title. [`assess`] itself allocates nothing, but a
/// [`Candidate`] and a [`Query`] built per reading clone both sides' alias, tag and author lists
/// each time — which made the search allocate in proportion to the square of a pair's synonyms
/// for values it then threw away. Both are now built once per direction.
///
/// # Why the winner is re-scored
///
/// The search runs on [`assess`], and only the winner is re-run through [`explain`] to itemise
/// it. Explaining every reading would allocate a term list per alias per direction for a number
/// the sweep discards.
fn best_reading(left: &SeriesMatchFacts, right: &SeriesMatchFacts, trigram: f32) -> PairReading {
    // score, whether the query side was the left series, the winning title, whether it is an alias
    let mut best: Option<(f32, bool, &str, bool)> = None;

    for query_is_left in [true, false] {
        let (query_side, candidate_side) = if query_is_left {
            (left, right)
        } else {
            (right, left)
        };
        let candidate = candidate_of(candidate_side, trigram);
        let mut query = query_of(query_side, String::new());

        // The canonical title goes first and ties are kept (the comparison is strict), so a pair
        // that both a canonical title and an alias explain is reported with the canonical
        // reading's signals.
        let titles = std::iter::once((query_side.normalized_title.as_str(), false)).chain(
            query_side
                .alt_normalized_titles
                .iter()
                .map(|alias| (alias.as_str(), true)),
        );
        for (title, from_alias) in titles {
            query.normalized_title.clear();
            query.normalized_title.push_str(title);
            let score = assess(&query, &candidate).score;
            if best.is_none_or(|(b, ..)| score > b) {
                best = Some((score, query_is_left, title, from_alias));
            }
        }
    }

    // At least one reading always exists: every series has a canonical normalized title.
    let (_, query_is_left, title, from_alias) =
        best.expect("a pair always has at least the two canonical readings");
    let (query_side, candidate_side) = if query_is_left {
        (left, right)
    } else {
        (right, left)
    };
    let query = query_of(query_side, title.to_owned());
    let candidate = candidate_of(candidate_side, trigram);
    let explained = relabel_alias_query(explain(&query, &candidate), from_alias);

    PairReading {
        assessment: explained.assessment,
        base: explained.base,
        terms: explained.terms,
        matched_query_title: title.to_owned(),
        matched_candidate_title: explained.matched_candidate_title,
        via_alias: explained.via_alias,
        query_is_left,
    }
}

fn query_of(facts: &SeriesMatchFacts, normalized_title: String) -> Query {
    Query {
        normalized_title,
        content_type: facts.content_type,
        release_year: facts.release_year,
        tags: facts.tags.clone(),
        authors: facts.authors.clone(),
    }
}

fn candidate_of(facts: &SeriesMatchFacts, similarity: f32) -> Candidate {
    Candidate {
        series_id: facts.series_id,
        normalized_title: facts.normalized_title.clone(),
        similarity,
        alt_normalized_titles: facts.alt_normalized_titles.clone(),
        content_type: facts.content_type,
        release_year: facts.release_year,
        tags: facts.tags.clone(),
        authors: facts.authors.clone(),
    }
}

/// Re-label an identity hit that came from one of the *query* side's alternative titles.
///
/// `exact_title` and `compact_identity` assert that the two series' **canonical** titles are the
/// same string. That claim is what the console renders as a badge and what the auto-merge journal
/// records as its justification for deleting a row, and it is false when the title that matched
/// was a synonym. `alias_identity` is the honest signal and is structural just the same, so the
/// verdict does not move — only the reason given for it.
fn relabel_alias_query(mut explanation: Explanation, query_from_alias: bool) -> Explanation {
    let signals = &mut explanation.assessment.signals;
    if query_from_alias && (signals.exact_title || signals.compact_identity) {
        signals.alias_identity = true;
        signals.exact_title = false;
        signals.compact_identity = false;
    }
    explanation
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

    fn verdict_for(a: &SeriesMatchFacts, b: &SeriesMatchFacts, trigram: f32) -> MergeVerdict {
        let reading = best_reading(a, b, trigram);
        adjudicate(reading.assessment, MatchingConfig::default().thresholds()).verdict
    }

    /// A run's totals sum every count except the one that means "still to do".
    ///
    /// `chains_deferred` is how many pairs the *last* pass skipped because of its own merges, and
    /// the sweep re-runs for exactly as long as it is non-zero. Summing it across passes would
    /// report the chains that later passes went on to resolve, so a run that fully collapsed a
    /// three-way duplicate would still read as having left work behind — and the counter an
    /// operator watches for a stuck chain would never come back to zero.
    #[test]
    fn a_multi_pass_run_reports_only_the_chains_the_last_pass_left() {
        let mut total = MergeSweepView::default();
        accumulate(
            &mut total,
            &MergeSweepView {
                pairs_examined: 10,
                auto_merged: 2,
                queued: 4,
                chains_deferred: 3,
                ..MergeSweepView::default()
            },
        );
        accumulate(
            &mut total,
            &MergeSweepView {
                pairs_examined: 4,
                auto_merged: 1,
                queued: 1,
                chains_deferred: 0,
                ..MergeSweepView::default()
            },
        );

        assert_eq!(total.pairs_examined, 14);
        assert_eq!(total.auto_merged, 3);
        assert_eq!(total.queued, 5);
        assert_eq!(
            total.chains_deferred, 0,
            "the chain the first pass deferred is the one the second resolved"
        );
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
            !best_reading(&bare_left, &right, 0.0)
                .assessment
                .signals
                .is_structural()
        );

        for (a, b) in [(&left, &right), (&right, &left)] {
            let reading = best_reading(a, b, 0.0);
            assert!(
                reading.assessment.signals.is_structural(),
                "{:?}",
                reading.assessment
            );
        }
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
        let reading = best_reading(&a, &b, 0.8);
        assert!(
            reading.assessment.signals.exact_title,
            "{:?}",
            reading.assessment
        );
        assert_eq!(verdict_for(&a, &b, 0.8), MergeVerdict::Auto);
    }

    /// The two `KunManga` listings the user reported, scored the way the sweep scores them.
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

        let reading = best_reading(&subtitled, &bare, 0.4);
        assert!(
            reading.assessment.signals.alias_identity,
            "{:?}",
            reading.assessment
        );
        assert_eq!(verdict_for(&subtitled, &bare, 0.4), MergeVerdict::Auto);
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
            !best_reading(&bare_left, &bare_right, 0.0)
                .assessment
                .signals
                .is_structural()
        );

        let reading = best_reading(&left, &right, 0.0);
        let signals = reading.assessment.signals;
        assert!(signals.alias_identity, "{signals:?}");
        assert!(
            !signals.exact_title && !signals.compact_identity,
            "a synonym hit must not claim the canonical titles are the same string: {signals:?}"
        );
    }

    /// A numbered sequel is never merged, however identical the rest of the title is.
    #[test]
    fn a_sequel_is_not_swept_up() {
        let a = facts("Kingdom of the Wind", &[], 1, 40);
        let b = facts("Kingdom of the Wind 2", &[], 1, 12);
        let reading = best_reading(&a, &b, 0.95);
        assert!(
            reading.assessment.signals.numeric_conflict,
            "{:?}",
            reading.assessment
        );
        assert_eq!(verdict_for(&a, &b, 0.95), MergeVerdict::Distinct);
    }

    /// Two works with the same title and no creator in common are held for review.
    ///
    /// The pair clears every part of the automatic bar — byte-identical titles, a score at the
    /// ceiling — and is exactly the shape of a remake, a spin-off, or an unrelated work that
    /// happens to share a name. Before the guard, the scorer had no term for disagreeing credits
    /// at all: `shared_author` was a bonus, and its absence was indistinguishable from a series
    /// with no credits recorded.
    #[test]
    fn same_title_by_different_creators_is_not_merged_automatically() {
        let mut a = facts("Phoenix", &[], 2, 40);
        a.authors = vec!["Osamu Tezuka".to_owned()];
        let mut b = facts("Phoenix", &[], 2, 30);
        b.authors = vec!["Someone Else".to_owned()];

        let reading = best_reading(&a, &b, 1.0);
        assert!(
            reading.assessment.signals.author_conflict,
            "{:?}",
            reading.assessment
        );
        assert!(
            reading.assessment.signals.is_structural(),
            "sanity: titles are identical"
        );

        let held = adjudicate(reading.assessment, MatchingConfig::default().thresholds());
        assert_eq!(held.verdict, MergeVerdict::Review);
        assert_eq!(held.blocked_by, vec!["author_conflict"]);

        // The same pair with one side's credits unknown says nothing either way, and merges.
        let bare = facts("Phoenix", &[], 2, 30);
        let quiet = best_reading(&a, &bare, 1.0);
        assert!(!quiet.assessment.signals.author_conflict);
        assert_eq!(verdict_for(&a, &bare, 1.0), MergeVerdict::Auto);
    }

    /// Every automatic merge carries the terms that produced its score.
    ///
    /// The journal is the whole point of the sweep being allowed to delete a row without asking:
    /// a stored score and a bag of signal names say *that* the pair matched, and the terms say
    /// which title matched and what each rule contributed. A reading with an empty term list is
    /// a merge nobody can audit.
    #[test]
    fn a_merged_pair_carries_its_itemised_score() {
        let a = facts("Spy X Family", &[], 3, 90);
        let b = facts("Spyxfamily", &[], 1, 12);
        let reading = best_reading(&a, &b, 0.4);

        assert_eq!(verdict_for(&a, &b, 0.4), MergeVerdict::Auto);
        assert!(
            reading.terms.iter().any(|t| t.rule == "base_similarity"),
            "the base the score started from must be recorded: {:?}",
            reading.terms
        );
        assert!(
            reading.terms.iter().any(|t| t.rule == "title_identity"),
            "the rule that justified the merge must be recorded: {:?}",
            reading.terms
        );
        // The score is reconstructible from the terms, which is what makes them an explanation
        // rather than a commentary.
        let summed: f32 = reading.terms.iter().map(|t| t.delta).sum();
        assert!(
            (summed.clamp(0.0, 1.0) - reading.assessment.score).abs() < 1e-5,
            "terms {:?} do not sum to {}",
            reading.terms,
            reading.assessment.score
        );
    }
}
